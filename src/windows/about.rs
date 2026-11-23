use ::windows::{
    Win32::{
        Foundation::{HWND, LPARAM, S_OK, WPARAM},
        UI::{
            Controls::{
                TASKDIALOG_NOTIFICATIONS, TASKDIALOGCONFIG, TASKDIALOGCONFIG_0, TDCBF_CLOSE_BUTTON,
                TDF_ALLOW_DIALOG_CANCELLATION, TDF_ENABLE_HYPERLINKS, TDF_USE_HICON_MAIN,
                TDN_HYPERLINK_CLICKED, TaskDialogIndirect,
            },
            WindowsAndMessaging::{MB_ICONWARNING, MB_OK, MessageBoxW},
        },
    },
    core::{HRESULT, HSTRING, PCWSTR, Result, w},
};

use super::{GITHUB_REPOSITORY, app_title, shell_open, ui_language, with_app};

pub fn open(owner: HWND) -> Result<()> {
    let icon = with_app(|app| app.tray.hIcon).unwrap_or_default();
    let language = ui_language();
    let version_label = language.choose("版本", "Version");
    let link_label = language.choose("访问 GitHub", "Visit GitHub");
    let content = HSTRING::from(format!(
        "{version_label} v{}\n\n<a href=\"https://github.com/{GITHUB_REPOSITORY}\">{link_label}</a>",
        env!("CARGO_PKG_VERSION")
    ));
    let dialog = TASKDIALOGCONFIG {
        cbSize: size_of::<TASKDIALOGCONFIG>() as u32,
        hwndParent: owner,
        dwFlags: TDF_ENABLE_HYPERLINKS | TDF_USE_HICON_MAIN | TDF_ALLOW_DIALOG_CANCELLATION,
        dwCommonButtons: TDCBF_CLOSE_BUTTON,
        pszWindowTitle: language.choose(w!("关于"), w!("About")),
        Anonymous1: TASKDIALOGCONFIG_0 { hMainIcon: icon },
        pszMainInstruction: PCWSTR(app_title().as_ptr()),
        pszContent: PCWSTR(content.as_ptr()),
        pfCallback: Some(dialog_callback),
        ..Default::default()
    };
    unsafe { TaskDialogIndirect(&dialog, None, None, None) }
}

unsafe extern "system" fn dialog_callback(
    hwnd: HWND,
    notification: TASKDIALOG_NOTIFICATIONS,
    _: WPARAM,
    link: LPARAM,
    _: isize,
) -> HRESULT {
    if notification == TDN_HYPERLINK_CLICKED {
        // Win32 supplies a null-terminated URL that remains valid during the callback.
        let url = PCWSTR(link.0 as *const u16);
        if let Err(error) = shell_open(hwnd, url) {
            let language = ui_language();
            let heading = language.choose("无法打开 GitHub。", "GitHub could not be opened.");
            let message = HSTRING::from(format!("{heading}\n\n{error}"));
            let title = language.choose(w!("关于"), w!("About"));
            unsafe { MessageBoxW(Some(hwnd), &message, title, MB_OK | MB_ICONWARNING) };
        }
    }
    S_OK
}
