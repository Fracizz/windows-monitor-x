//! 菜单、管理窗口与面板交互触发的命令，以及更新、导出等异步流程的收尾。

use super::*;
use worker::history_directory;

pub(super) fn double_click_labels(language: Language) -> [PCWSTR; DOUBLE_CLICK_OPTIONS.len()] {
    language.choose(
        [
            w!("无操作"),
            w!("打开任务管理器"),
            w!("打开资源监视器"),
            w!("打开管理窗口"),
        ],
        [
            w!("Do Nothing"),
            w!("Open Task Manager"),
            w!("Open Resource Monitor"),
            w!("Open Manager"),
        ],
    )
}

pub(super) fn double_click(hwnd: HWND) {
    let Some(action) =
        with_app(|app| (!app.waiting_sample).then_some(app.settings.double_click)).flatten()
    else {
        return;
    };
    let (program, heading) = match action {
        DoubleClick::Nothing => return,
        DoubleClick::Manager => return command(hwnd, OPEN_MANAGER),
        DoubleClick::TaskManager => (
            w!("taskmgr.exe"),
            ui_language().choose("无法打开任务管理器。", "Task Manager could not be opened."),
        ),
        DoubleClick::ResourceMonitor => (
            w!("resmon.exe"),
            ui_language().choose(
                "无法打开资源监视器。",
                "Resource Monitor could not be opened.",
            ),
        ),
    };
    match shell_open(hwnd, program) {
        // 用户在 UAC 提示中选择“否”，不是错误。
        Err(error) if error.code() != ERROR_CANCELLED.to_hresult() => {
            show_warning(Some(manager::dialog_owner(hwnd)), heading, &error);
        }
        _ => {}
    }
}

/// 通过 Shell 打开文件、程序或网址；需要提权的程序（如任务管理器）由 Shell 负责处理 UAC。
pub(super) fn shell_open(hwnd: HWND, target: PCWSTR) -> Result<()> {
    let mut execute = SHELLEXECUTEINFOW {
        cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_FLAG_NO_UI,
        hwnd,
        lpVerb: w!("open"),
        lpFile: target,
        nShow: SW_SHOWNORMAL.0,
        ..Default::default()
    };
    unsafe {
        CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE).ok()?;
        let result = ShellExecuteExW(&mut execute);
        CoUninitialize();
        result
    }
}

pub(super) fn set_display(hwnd: HWND, display: DisplayOptions) {
    let apply_now = with_app(|app| {
        if app.settings.display == display {
            return false;
        }
        let size = Layout::Taskbar.size(display);
        let resize_dock =
            app.settings.taskbar && size != Layout::Taskbar.size(app.settings.display);
        app.settings.display = display;
        if resize_dock {
            app.request_dock();
        }
        app.save();
        // Keep the current panel in its slot until the worker has checked the new dimensions.
        app.layout() != Layout::Taskbar || size == Layout::Taskbar.size(app.display)
    });
    if apply_now == Some(true) {
        apply_placement(hwnd);
    }
}

fn set_language(hwnd: HWND, preference: LanguagePreference) {
    let language = preference.resolve();
    let display = with_app(|app| {
        let previous = app.settings.display;
        if app.settings.language == preference && previous.language == language {
            return None;
        }
        app.settings.language = preference;
        if previous.language == language {
            app.save();
            return None;
        }
        Some(DisplayOptions {
            language,
            ..previous
        })
    })
    .flatten();
    if let Some(display) = display {
        set_thread_language(language);
        set_display(hwnd, display);
        editor::set_language(language);
    }
}

fn start_update(hwnd: HWND, action: updater::Action) {
    let result = with_app(|app| {
        if app.update.is_none() {
            app.update = Some(updater::start(hwnd, action, app.settings.display.language)?);
        }
        Ok::<_, std::io::Error>(())
    });
    if let Some(Err(error)) = result {
        show_update_error(Some(manager::dialog_owner(hwnd)), &error);
    }
}

pub(super) fn finish_update(hwnd: HWND) {
    let event = with_app(|app| {
        let event = app.update.as_ref()?.try_recv().ok()?;
        app.update = None;
        Some(event)
    })
    .flatten();
    let owner = manager::dialog_owner(hwnd);
    match event {
        Some(updater::Event::Checked(Ok(Some(release)))) => {
            let version = release.version.to_string();
            let text = HSTRING::from(match ui_language() {
                Language::Chinese => format!("GitHub 上有新版本 v{version}。是否下载并重启应用？"),
                Language::English => format!(
                    "Version v{version} is available on GitHub. Download it and restart the app?"
                ),
            });
            let answer = unsafe {
                MessageBoxW(
                    Some(owner),
                    &text,
                    app_title(),
                    MB_YESNO | MB_ICONQUESTION | MB_DEFBUTTON2,
                )
            };
            if answer == IDYES {
                start_update(hwnd, updater::Action::Download(release));
            }
        }
        Some(updater::Event::Checked(Ok(None))) => {
            let text = HSTRING::from(ui_language().choose(
                concat!("当前已是最新版本 v", env!("CARGO_PKG_VERSION"), "。"),
                concat!(
                    "You are using the latest version, v",
                    env!("CARGO_PKG_VERSION"),
                    "."
                ),
            ));
            unsafe { MessageBoxW(Some(owner), &text, app_title(), MB_OK | MB_ICONINFORMATION) };
        }
        Some(updater::Event::Downloaded(Ok(update))) => match update.launch(ui_language()) {
            Ok(()) => {
                let _ = unsafe { DestroyWindow(hwnd) };
            }
            Err(error) => show_update_error(Some(owner), &error),
        },
        Some(updater::Event::Checked(Err(error)) | updater::Event::Downloaded(Err(error))) => {
            show_update_error(Some(owner), &error);
        }
        None => {}
    }
    manager::refresh_settings();
}

pub(super) fn show_update_error(hwnd: Option<HWND>, error: &impl std::fmt::Display) {
    show_warning(
        hwnd,
        ui_language().choose("无法完成在线更新。", "The update could not be completed."),
        error,
    );
}

fn show_warning(hwnd: Option<HWND>, heading: &str, error: &impl std::fmt::Display) {
    let text = HSTRING::from(format!("{heading}\n\n{error}"));
    unsafe { MessageBoxW(hwnd, &text, app_title(), MB_OK | MB_ICONWARNING) };
}

fn start_history_export(hwnd: HWND, owner: HWND) {
    let Some(feed) = with_app(|app| {
        if app.history_exporting {
            return None;
        }
        app.history_exporting = true;
        Some(app.feed.clone())
    })
    .flatten() else {
        return;
    };
    let language = ui_language();
    let started = (|| -> io::Result<bool> {
        let Some(path) = history_export::destination(owner, language)? else {
            return Ok(false);
        };
        let directory = history_directory()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "LOCALAPPDATA"))?;
        let handle = hwnd.0 as usize;
        thread::Builder::new()
            .name("monitor-history-export".into())
            .spawn(move || {
                let result = history::export(&directory, &path, history::now_ms());
                feed.lock().unwrap().history_export = Some(result);
                notify(HWND(handle as *mut _), HISTORY_EXPORTED);
            })?;
        Ok(true)
    })();
    if !matches!(started, Ok(true)) {
        with_app(|app| app.history_exporting = false);
    }
    if let Err(error) = started {
        show_history_export_error(owner, &error);
    }
}

pub(super) fn finish_history_export(hwnd: HWND) {
    let result = with_app(|app| {
        let result = app.feed.lock().unwrap().history_export.take();
        if result.is_some() {
            app.history_exporting = false;
        }
        result
    })
    .flatten();
    manager::refresh_settings();
    let owner = manager::dialog_owner(hwnd);
    match result {
        Some(Ok(count)) => {
            let text = HSTRING::from(ui_language().choose(
                format!("已导出 {count} 条监控记录。"),
                format!("Exported {count} monitoring samples."),
            ));
            unsafe { MessageBoxW(Some(owner), &text, app_title(), MB_OK | MB_ICONINFORMATION) };
        }
        Some(Err(error)) => show_history_export_error(owner, &error),
        None => {}
    }
}

fn show_history_export_error(owner: HWND, error: &io::Error) {
    show_warning(
        Some(owner),
        ui_language().choose("无法导出监控记录。", "Could not export monitoring history."),
        error,
    );
}

pub(super) fn command(hwnd: HWND, selection: usize) {
    let owner = manager::dialog_owner(hwnd);
    match selection {
        OPEN_MANAGER => {
            if let Err(error) = manager::open(hwnd) {
                show_warning(
                    Some(hwnd),
                    ui_language().choose(
                        "无法打开管理窗口。",
                        "The manager window could not be opened.",
                    ),
                    &error,
                );
            }
            return;
        }
        EXPORT_HISTORY => start_history_export(hwnd, owner),
        RECORD_HISTORY => {
            with_app(|app| {
                app.settings.record_history = !app.settings.record_history;
                let _ = app
                    .worker
                    .send(WorkerCommand::RecordHistory(app.settings.record_history));
                app.save();
            });
        }
        ABOUT => {
            if let Err(error) = about::open(owner) {
                show_warning(
                    Some(owner),
                    ui_language().choose(
                        "无法打开关于窗口。",
                        "The About dialog could not be opened.",
                    ),
                    &error,
                );
            }
        }
        CHECK_UPDATE => start_update(hwnd, updater::Action::Check),
        EDIT_METRICS => {
            if let Err(error) = editor::open(hwnd, owner) {
                show_warning(
                    Some(owner),
                    ui_language().choose(
                        "无法打开监控项编辑器。",
                        "The metrics editor could not be opened.",
                    ),
                    &error,
                );
            }
        }
        AUTOSTART => {
            let language = ui_language();
            if let Err(error) = startup::toggle(language) {
                show_warning(
                    Some(owner),
                    language.choose(
                        "无法更改开机自启设置。",
                        "The run-at-sign-in setting could not be changed.",
                    ),
                    &error,
                );
            }
        }
        LANGUAGE_SYSTEM | LANGUAGE_CHINESE | LANGUAGE_ENGLISH => {
            set_language(
                hwnd,
                match selection {
                    LANGUAGE_CHINESE => LanguagePreference::Chinese,
                    LANGUAGE_ENGLISH => LanguagePreference::English,
                    _ => LanguagePreference::System,
                },
            );
        }
        TOGGLE => {
            let changed = with_app(|app| {
                if app.tray_ready {
                    app.shown = !app.shown;
                }
                app.tray_ready
            });
            if changed == Some(true) {
                apply_visibility(hwnd);
            }
        }
        DOCK => {
            with_app(|app| {
                app.settings.taskbar = !app.settings.taskbar;
                app.request_dock();
                app.placement = (!app.settings.taskbar).then_some(Placement::Floating);
                app.save();
            });
            apply_placement(hwnd);
        }
        DOCK_LEFT | DOCK_RIGHT => {
            let side = if selection == DOCK_LEFT {
                TaskbarSide::Left
            } else {
                TaskbarSide::Right
            };
            with_app(|app| {
                if app.settings.taskbar_side != side {
                    app.settings.taskbar_side = side;
                    if app.settings.taskbar {
                        app.request_dock();
                    }
                    app.save();
                }
            });
        }
        TOPMOST => {
            if let Some(topmost) = with_app(|app| {
                app.settings.topmost = !app.settings.topmost;
                app.save();
                app.panel_topmost()
            }) {
                set_topmost(hwnd, topmost);
            }
        }
        FOLLOW_SYSTEM | LIGHT | DARK | TRANSPARENT_THEME => {
            with_app(|app| {
                app.settings.theme = match selection {
                    LIGHT => Theme::Light,
                    DARK => Theme::Dark,
                    TRANSPARENT_THEME => Theme::Transparent,
                    _ => Theme::System,
                };
                app.save();
            });
            apply_placement(hwnd);
            editor::refresh_preview();
        }
        LABEL_BOLD => {
            with_app(|app| {
                app.settings.label_bold = !app.settings.label_bold;
                app.save();
            });
            invalidate(hwnd);
            editor::refresh_preview();
        }
        RESET_TRAFFIC => {
            with_app(|app| {
                app.take_sample();
                app.starting_traffic = 0;
                app.traffic_at_reset = app.sample.traffic;
                let _ = app
                    .worker
                    .send(WorkerCommand::ResetHistoryTraffic(app.traffic_at_reset));
                app.update_display();
                app.save();
            });
            invalidate(hwnd);
            editor::refresh_preview();
        }
        EXIT => {
            let _ = unsafe { DestroyWindow(hwnd) };
            return;
        }
        _ => {
            if let Some((_, interval)) =
                REFRESH_OPTIONS.into_iter().find(|(id, _)| *id == selection)
            {
                with_app(|app| {
                    app.settings.refresh = interval;
                    let _ = app.worker.send(WorkerCommand::Interval(interval));
                    app.save();
                });
            } else if let Some((_, action)) = DOUBLE_CLICK_OPTIONS
                .into_iter()
                .find(|(id, _)| *id == selection)
            {
                with_app(|app| {
                    app.settings.double_click = action;
                    app.save();
                });
            }
        }
    }
    manager::refresh_settings();
}
