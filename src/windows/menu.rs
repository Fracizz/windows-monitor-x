//! 托盘与面板的右键菜单。

use super::*;

pub(super) struct Menu(HMENU);

impl Menu {
    fn new() -> Result<Self> {
        Ok(Self(unsafe { CreatePopupMenu()? }))
    }

    fn item(&self, id: usize, label: PCWSTR, flags: MENU_ITEM_FLAGS) -> Result<()> {
        unsafe { AppendMenuW(self.0, MF_STRING | flags, id, label) }
    }

    fn separator(&self) -> Result<()> {
        unsafe { AppendMenuW(self.0, MF_SEPARATOR, 0, PCWSTR::null()) }
    }

    fn submenu(&self, label: PCWSTR, child: Self) -> Result<()> {
        let handle = child.0;
        unsafe { AppendMenuW(self.0, MF_POPUP | MF_STRING, handle.0 as usize, label)? };
        // The parent menu now owns the submenu.
        std::mem::forget(child);
        Ok(())
    }
}

impl Drop for Menu {
    fn drop(&mut self) {
        let _ = unsafe { DestroyMenu(self.0) };
    }
}

pub(super) fn open_menu(hwnd: HWND, at: POINT) {
    let Some((settings, tray_ready, shown, placement, update_busy)) = with_app(|app| {
        (
            app.settings,
            app.tray_ready,
            app.shown,
            app.placement.unwrap_or_default(),
            app.update.is_some(),
        )
    }) else {
        return;
    };
    if let Ok(menu) = make_menu(
        settings,
        tray_ready,
        shown,
        placement,
        startup::status().ok(),
        update_busy,
    ) {
        let menu_host = (settings.theme == Theme::Transparent)
            .then(|| create_menu_host().ok())
            .flatten();
        let owner = menu_host.as_ref().map_or(hwnd, |window| window.0);
        with_app(|app| app.menu_open = true);
        let selection = unsafe {
            let _ = SetForegroundWindow(owner);
            let result = TrackPopupMenu(
                menu.0,
                TPM_RIGHTBUTTON | TPM_RETURNCMD | TPM_NONOTIFY,
                at.x,
                at.y,
                None,
                owner,
                None,
            );
            let _ = PostMessageW(Some(owner), WM_NULL, WPARAM(0), LPARAM(0));
            result.0 as usize
        };
        with_app(|app| app.menu_open = false);
        if selection != 0 {
            command(hwnd, selection);
        }
    }
}

fn create_menu_host() -> Result<Window> {
    // Keep the painted panel out of the foreground while Windows tracks its menu.
    unsafe {
        let instance = HINSTANCE(GetModuleHandleW(PCWSTR::null())?.0);
        let window = Window(CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_NOREDIRECTIONBITMAP,
            w!("Static"),
            PCWSTR::null(),
            WS_POPUP,
            GetSystemMetrics(SM_XVIRTUALSCREEN).saturating_sub(32),
            GetSystemMetrics(SM_YVIRTUALSCREEN).saturating_sub(32),
            1,
            1,
            None,
            None,
            Some(instance),
            None,
        )?);
        set_panel_frame(window.0, false);
        let _ = ShowWindow(window.0, SW_SHOWNOACTIVATE);
        Ok(window)
    }
}

pub(super) fn make_menu(
    settings: Settings,
    tray_ready: bool,
    shown: bool,
    placement: Placement,
    startup: Option<startup::Status>,
    update_busy: bool,
) -> Result<Menu> {
    let language = settings.display.language;
    let menu = Menu::new()?;
    menu.item(
        OPEN_MANAGER,
        language.choose(w!("打开管理窗口…"), w!("Open Manager…")),
        MF_ENABLED,
    )?;
    let visibility = if shown {
        language.choose(w!("隐藏监控"), w!("Hide Monitor"))
    } else {
        language.choose(w!("显示监控"), w!("Show Monitor"))
    };
    let available = if tray_ready { MF_ENABLED } else { MF_GRAYED };
    menu.item(TOGGLE, visibility, available)?;
    menu.submenu(
        language.choose(w!("设置"), w!("Settings")),
        make_settings_menu(settings, placement, startup, update_busy)?,
    )?;
    menu.separator()?;
    menu.item(
        EXPORT_HISTORY,
        language.choose(w!("导出监控记录…"), w!("Export History…")),
        MF_ENABLED,
    )?;
    menu.item(
        RESET_TRAFFIC,
        language.choose(w!("清零总流量"), w!("Reset Total Traffic")),
        MF_ENABLED,
    )?;
    menu.item(
        ABOUT,
        language.choose(w!("关于…"), w!("About…")),
        MF_ENABLED,
    )?;
    menu.item(EXIT, language.choose(w!("退出"), w!("Exit")), MF_ENABLED)?;
    Ok(menu)
}

fn make_settings_menu(
    settings: Settings,
    placement: Placement,
    startup: Option<startup::Status>,
    update_busy: bool,
) -> Result<Menu> {
    let language = settings.display.language;
    let menu = Menu::new()?;
    let checked = |condition| if condition { MF_CHECKED } else { MF_UNCHECKED };
    let startup_label = if startup == Some(startup::Status::OtherPath) {
        language.choose(
            w!("开机自启（更新路径）"),
            w!("Run at Sign-in (Update Path)"),
        )
    } else {
        language.choose(w!("开机自启"), w!("Run at Sign-in"))
    };
    let docked = matches!(placement, Placement::Docked(_));
    let dock_label = if settings.taskbar && placement == Placement::Floating {
        language.choose(
            w!("嵌入任务栏（等候空位）"),
            w!("Embed in Taskbar (Waiting for Space)"),
        )
    } else {
        language.choose(w!("嵌入任务栏"), w!("Embed in Taskbar"))
    };
    let topmost = if docked { MF_GRAYED } else { MF_ENABLED };
    menu.item(
        AUTOSTART,
        startup_label,
        checked(startup == Some(startup::Status::Enabled)),
    )?;
    menu.item(DOCK, dock_label, checked(settings.taskbar))?;
    menu.item(
        TOPMOST,
        language.choose(w!("窗口置顶"), w!("Always on Top")),
        topmost | checked(settings.topmost || docked),
    )?;
    menu.separator()?;

    let refresh = Menu::new()?;
    let labels = language.choose(
        [w!("1 秒"), w!("2 秒"), w!("3 秒"), w!("5 秒")],
        [
            w!("1 second"),
            w!("2 seconds"),
            w!("3 seconds"),
            w!("5 seconds"),
        ],
    );
    for ((id, interval), label) in REFRESH_OPTIONS.into_iter().zip(labels) {
        refresh.item(id, label, checked(settings.refresh == interval))?;
    }
    menu.submenu(
        language.choose(w!("刷新频率"), w!("Refresh Interval")),
        refresh,
    )?;

    let theme = Menu::new()?;
    for (id, choice, label) in [
        (
            FOLLOW_SYSTEM,
            Theme::System,
            language.choose(w!("跟随系统"), w!("Use System Theme")),
        ),
        (
            LIGHT,
            Theme::Light,
            language.choose(w!("浅色"), w!("Light")),
        ),
        (DARK, Theme::Dark, language.choose(w!("深色"), w!("Dark"))),
        (
            TRANSPARENT_THEME,
            Theme::Transparent,
            language.choose(w!("透明"), w!("Transparent")),
        ),
    ] {
        theme.item(id, label, checked(settings.theme == choice))?;
    }
    menu.submenu(language.choose(w!("主题"), w!("Theme")), theme)?;
    menu.item(
        LABEL_BOLD,
        language.choose(w!("标签加粗"), w!("Bold Labels")),
        checked(settings.label_bold),
    )?;

    let taskbar_position = Menu::new()?;
    taskbar_position.item(
        DOCK_LEFT,
        language.choose(w!("靠左"), w!("Left")),
        checked(settings.taskbar_side == TaskbarSide::Left),
    )?;
    taskbar_position.item(
        DOCK_RIGHT,
        language.choose(w!("靠右"), w!("Right")),
        checked(settings.taskbar_side == TaskbarSide::Right),
    )?;
    menu.submenu(
        language.choose(w!("任务栏位置"), w!("Taskbar Position")),
        taskbar_position,
    )?;
    let double_click = Menu::new()?;
    for ((id, action), label) in DOUBLE_CLICK_OPTIONS
        .into_iter()
        .zip(double_click_labels(language))
    {
        double_click.item(id, label, checked(settings.double_click == action))?;
    }
    menu.submenu(
        language.choose(w!("双击面板"), w!("Double-Click Panel")),
        double_click,
    )?;
    menu.item(
        EDIT_METRICS,
        language.choose(w!("编辑监控项…"), w!("Edit Metrics…")),
        MF_ENABLED,
    )?;
    menu.item(
        RECORD_HISTORY,
        language.choose(w!("记录监控数据"), w!("Record History")),
        checked(settings.record_history),
    )?;
    let (update_label, update_flags) = if update_busy {
        (
            language.choose(w!("正在处理更新…"), w!("Working on Update…")),
            MF_GRAYED,
        )
    } else {
        (
            language.choose(w!("检查更新…"), w!("Check for Updates…")),
            MF_ENABLED,
        )
    };
    menu.item(CHECK_UPDATE, update_label, update_flags)?;
    let languages = Menu::new()?;
    for (id, preference, label) in [
        (
            LANGUAGE_SYSTEM,
            LanguagePreference::System,
            language.choose(w!("跟随系统"), w!("Use System Language")),
        ),
        (
            LANGUAGE_CHINESE,
            LanguagePreference::Chinese,
            w!("简体中文"),
        ),
        (LANGUAGE_ENGLISH, LanguagePreference::English, w!("English")),
    ] {
        languages.item(id, label, checked(settings.language == preference))?;
    }
    menu.submenu(language.choose(w!("语言"), w!("Language")), languages)?;
    Ok(menu)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_menu_groups_preferences_and_shows_current_choices() {
        let settings = Settings {
            refresh: Refresh::Three,
            theme: Theme::Transparent,
            ..Default::default()
        };
        let menu = make_menu(
            settings,
            true,
            true,
            Placement::Floating,
            Some(startup::Status::Enabled),
            false,
        )
        .unwrap();
        unsafe {
            assert_eq!(GetMenuItemCount(Some(menu.0)), 8);
            assert_eq!(GetMenuItemID(menu.0, 0), OPEN_MANAGER as u32);
            assert_eq!(GetMenuItemID(menu.0, 1), TOGGLE as u32);
            assert_eq!(GetMenuItemID(menu.0, 4), EXPORT_HISTORY as u32);
            assert_eq!(GetMenuItemID(menu.0, 6), ABOUT as u32);
            assert_eq!(GetMenuItemID(menu.0, 7), EXIT as u32);
            let settings = GetSubMenu(menu.0, 2);
            assert!(!settings.is_invalid());
            let refresh = GetSubMenu(settings, 4);
            let theme = GetSubMenu(settings, 5);
            let position = GetSubMenu(settings, 7);
            assert_eq!(GetMenuItemID(settings, 6), LABEL_BOLD as u32);
            let double_click = GetSubMenu(settings, 8);
            assert!(!double_click.is_invalid());
            assert_eq!(GetMenuItemID(settings, 9), EDIT_METRICS as u32);
            assert_eq!(GetMenuItemID(settings, 10), RECORD_HISTORY as u32);
            assert_eq!(GetMenuItemID(settings, 11), CHECK_UPDATE as u32);
            assert!(!refresh.is_invalid());
            assert!(!theme.is_invalid());
            assert!(!position.is_invalid());
            for (submenu, command) in [
                (settings, AUTOSTART),
                (settings, LABEL_BOLD),
                (refresh, 111),
                (theme, TRANSPARENT_THEME),
                (position, DOCK_RIGHT),
                (double_click, 128),
            ] {
                let flags = GetMenuState(submenu, command as u32, MF_BYCOMMAND);
                assert_ne!(flags, u32::MAX);
                assert_ne!(flags & MF_CHECKED.0, 0);
            }
        }
    }
}
