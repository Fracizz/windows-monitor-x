use std::{ffi::OsString, io, os::windows::ffi::OsStringExt, path::PathBuf};

use ::windows::{
    Win32::{
        Foundation::HWND,
        UI::Controls::Dialogs::{
            CommDlgExtendedError, GetSaveFileNameW, OFN_ENABLESIZING, OFN_EXPLORER,
            OFN_NOCHANGEDIR, OFN_OVERWRITEPROMPT, OFN_PATHMUSTEXIST, OPENFILENAMEW,
        },
    },
    core::{PWSTR, w},
};

use crate::language::Language;

pub fn destination(owner: HWND, language: Language) -> io::Result<Option<PathBuf>> {
    let mut path = vec![0u16; 32_768];
    for (target, unit) in path
        .iter_mut()
        .zip("windows-monitor-history.json".encode_utf16())
    {
        *target = unit;
    }
    let mut dialog = OPENFILENAMEW {
        lStructSize: size_of::<OPENFILENAMEW>() as u32,
        hwndOwner: owner,
        lpstrFilter: w!("JSON (*.json)\0*.json\0"),
        nFilterIndex: 1,
        lpstrFile: PWSTR(path.as_mut_ptr()),
        nMaxFile: path.len() as u32,
        lpstrTitle: language.choose(w!("导出监控记录"), w!("Export Monitoring History")),
        lpstrDefExt: w!("json"),
        Flags: OFN_EXPLORER
            | OFN_ENABLESIZING
            | OFN_NOCHANGEDIR
            | OFN_OVERWRITEPROMPT
            | OFN_PATHMUSTEXIST,
        ..Default::default()
    };
    if !unsafe { GetSaveFileNameW(&mut dialog) }.as_bool() {
        let error = unsafe { CommDlgExtendedError() }.0;
        return if error == 0 {
            Ok(None)
        } else {
            Err(io::Error::other(language.choose(
                format!("保存对话框错误 0x{error:04X}"),
                format!("Save dialog error 0x{error:04X}"),
            )))
        };
    }
    let end = path
        .iter()
        .position(|&unit| unit == 0)
        .unwrap_or(path.len());
    let destination = PathBuf::from(OsString::from_wide(&path[..end]));
    if !destination
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("json"))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            language.choose(
                "请选择以 .json 结尾的文件名。",
                "Choose a file name ending in .json.",
            ),
        ));
    }
    Ok(Some(destination))
}
