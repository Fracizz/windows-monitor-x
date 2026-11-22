use ::windows::{
    Win32::{
        Foundation::{HWND, LPARAM, S_OK, WPARAM},
        System::Com::{
            COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx, CoUninitialize,
        },
        UI::{
            Controls::{
                TASKDIALOG_NOTIFICATIONS, TASKDIALOGCONFIG, TASKDIALOGCONFIG_0, TDCBF_CLOSE_BUTTON,
                TDF_ALLOW_DIALOG_CANCELLATION, TDF_ENABLE_HYPERLINKS, TDF_USE_HICON_MAIN,
                TDN_HYPERLINK_CLICKED, TaskDialogIndirect,
            },
            Shell::{SEE_MASK_FLAG_NO_UI, SHELLEXECUTEINFOW, ShellExecuteExW},
            WindowsAndMessaging::{MB_ICONWARNING, MB_OK, MessageBoxW, SW_SHOWNORMAL},
        },
    },
    core::{HRESULT, HSTRING, PCWSTR, Result, w},
};

use super::{GITHUB_REPOSITORY, app_title, with_app};

pub fn open(owner: HWND) -> Result<()> {
    let icon = with_app(|app| app.tray.hIcon).unwrap_or_default();
    let content = HSTRING::from(format!(
        "版本 v{}\n\n<a href=\"https://github.com/{GITHUB_REPOSITORY}\">访问 GitHub</a>",
        env!("CARGO_PKG_VERSION")
    ));
    let dialog = TASKDIALOGCONFIG {
        cbSize: size_of::<TASKDIALOGCONFIG>() as u32,
        hwndParent: owner,
        dwFlags: TDF_ENABLE_HYPERLINKS | TDF_USE_HICON_MAIN | TDF_ALLOW_DIALOG_CANCELLATION,
        dwCommonButtons: TDCBF_CLOSE_BUTTON,
        pszWindowTitle: w!("关于"),
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
        if let Err(error) = unsafe { open_in_browser(hwnd, url) } {
            let message = HSTRING::from(format!("无法打开 GitHub。\n\n{error}"));
            unsafe { MessageBoxW(Some(hwnd), &message, w!("关于"), MB_OK | MB_ICONWARNING) };
        }
    }
    S_OK
}

unsafe fn open_in_browser(hwnd: HWND, url: PCWSTR) -> Result<()> {
    let mut execute = SHELLEXECUTEINFOW {
        cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_FLAG_NO_UI,
        hwnd,
        lpVerb: w!("open"),
        lpFile: url,
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
