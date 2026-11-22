use std::{env, io, os::windows::ffi::OsStrExt, path::Path};

use ::windows::{
    Win32::{
        Foundation::{
            ERROR_FILE_NOT_FOUND, ERROR_MORE_DATA, ERROR_SUCCESS, ERROR_UNSUPPORTED_TYPE,
        },
        System::Registry::{
            HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_SZ, RegDeleteKeyValueW, RegGetValueW,
            RegSetKeyValueW,
        },
    },
    core::{PCWSTR, w},
};

const RUN_KEY: PCWSTR = w!(r"Software\Microsoft\Windows\CurrentVersion\Run");
const VALUE_NAME: PCWSTR = w!("WindowsMonitor");
const RUN_COMMAND_LIMIT: usize = 260;
pub const BACKGROUND_ARG: &str = "--background";

use crate::language::Language;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Status {
    Disabled,
    Enabled,
    OtherPath,
}

pub fn status() -> io::Result<Status> {
    status_at(RUN_KEY, &quoted_path(&env::current_exe()?))
}

pub fn toggle(language: Language) -> io::Result<()> {
    toggle_at(RUN_KEY, &quoted_path(&env::current_exe()?), language)
}

pub fn migrate() -> io::Result<()> {
    migrate_at(RUN_KEY, &quoted_path(&env::current_exe()?))
}

fn quoted_path(path: &Path) -> Vec<u16> {
    [u16::from(b'"')]
        .into_iter()
        .chain(path.as_os_str().encode_wide())
        .chain([u16::from(b'"')])
        .collect()
}

fn background_command(path: &[u16]) -> Vec<u16> {
    path.iter()
        .copied()
        .chain([u16::from(b' ')])
        .chain(BACKGROUND_ARG.encode_utf16())
        .chain([0])
        .collect()
}

#[derive(PartialEq)]
enum Registration {
    Missing,
    Current,
    Legacy,
    OtherPath,
}

fn registration_at(key: PCWSTR, path: &[u16]) -> io::Result<Registration> {
    let mut stored = [0u16; RUN_COMMAND_LIMIT + 1];
    let mut bytes = size_of_val(&stored) as u32;
    let result = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            key,
            VALUE_NAME,
            RRF_RT_REG_SZ,
            None,
            Some(stored.as_mut_ptr().cast()),
            Some(&mut bytes),
        )
    };
    match result {
        ERROR_FILE_NOT_FOUND => Ok(Registration::Missing),
        ERROR_MORE_DATA | ERROR_UNSUPPORTED_TYPE => Ok(Registration::OtherPath),
        ERROR_SUCCESS => {
            let stored = stored.get(..bytes as usize / size_of::<u16>());
            if stored == Some(background_command(path).as_slice()) {
                Ok(Registration::Current)
            } else if stored.and_then(|value| value.strip_suffix(&[0])) == Some(path) {
                Ok(Registration::Legacy)
            } else {
                Ok(Registration::OtherPath)
            }
        }
        error => Err(io::Error::from_raw_os_error(error.0 as i32)),
    }
}

fn status_at(key: PCWSTR, path: &[u16]) -> io::Result<Status> {
    Ok(match registration_at(key, path)? {
        Registration::Missing => Status::Disabled,
        Registration::Current | Registration::Legacy => Status::Enabled,
        Registration::OtherPath => Status::OtherPath,
    })
}

fn migrate_at(key: PCWSTR, path: &[u16]) -> io::Result<()> {
    if registration_at(key, path)? == Registration::Legacy {
        let command = background_command(path);
        if command.len() <= RUN_COMMAND_LIMIT + 1 {
            write_command(key, &command)?;
        }
    }
    Ok(())
}

fn toggle_at(key: PCWSTR, path: &[u16], language: Language) -> io::Result<()> {
    if status_at(key, path)? == Status::Enabled {
        let result = unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, key, VALUE_NAME) };
        if result == ERROR_SUCCESS || result == ERROR_FILE_NOT_FOUND {
            Ok(())
        } else {
            Err(io::Error::from_raw_os_error(result.0 as i32))
        }
    } else {
        let command = background_command(path);
        if command.len() > RUN_COMMAND_LIMIT + 1 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                language.choose(
                    "程序路径过长。请将程序移到较短的路径，再启用自启。",
                    "The app path is too long. Move the app to a shorter path before enabling run at sign-in.",
                ),
            ));
        }
        write_command(key, &command)
    }
}

fn write_command(key: PCWSTR, command: &[u16]) -> io::Result<()> {
    let result = unsafe {
        RegSetKeyValueW(
            HKEY_CURRENT_USER,
            key,
            VALUE_NAME,
            REG_SZ.0,
            Some(command.as_ptr().cast()),
            size_of_val(command) as u32,
        )
    };
    if result == ERROR_SUCCESS {
        Ok(())
    } else {
        Err(io::Error::from_raw_os_error(result.0 as i32))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::windows::Win32::System::Registry::RegDeleteKeyW;

    struct TestKey(Vec<u16>);

    impl Drop for TestKey {
        fn drop(&mut self) {
            let _ = unsafe { RegDeleteKeyW(HKEY_CURRENT_USER, PCWSTR(self.0.as_ptr())) };
        }
    }

    #[test]
    fn startup_can_be_enabled_moved_and_disabled() {
        let key_name = format!(r"Software\WindowsMonitorStartupTest-{}", std::process::id());
        let owned_key = TestKey(key_name.encode_utf16().chain([0]).collect());
        let key = PCWSTR(owned_key.0.as_ptr());
        let path = Path::new(r"D:\便携 工具\windows-monitor.exe");
        let command = quoted_path(path);
        assert_eq!(
            String::from_utf16(&command).unwrap(),
            format!("\"{}\"", path.display())
        );
        let background = background_command(&command);
        assert_eq!(
            String::from_utf16(&background[..background.len() - 1]).unwrap(),
            format!("\"{}\" {BACKGROUND_ARG}", path.display())
        );

        let moved_command = quoted_path(Path::new(r"D:\新目录\windows-monitor.exe"));
        assert_eq!(status_at(key, &command).unwrap(), Status::Disabled);
        let mut legacy = command.clone();
        legacy.push(0);
        write_command(key, &legacy).unwrap();
        assert_eq!(status_at(key, &command).unwrap(), Status::Enabled);
        migrate_at(key, &moved_command).unwrap();
        assert!(registration_at(key, &command).unwrap() == Registration::Legacy);
        migrate_at(key, &command).unwrap();
        assert!(registration_at(key, &command).unwrap() == Registration::Current);
        toggle_at(key, &command, Language::English).unwrap();
        toggle_at(key, &command, Language::English).unwrap();
        assert_eq!(status_at(key, &command).unwrap(), Status::Enabled);
        assert_eq!(status_at(key, &moved_command).unwrap(), Status::OtherPath);
        toggle_at(key, &moved_command, Language::English).unwrap();
        assert_eq!(status_at(key, &moved_command).unwrap(), Status::Enabled);
        toggle_at(key, &moved_command, Language::English).unwrap();
        assert_eq!(status_at(key, &moved_command).unwrap(), Status::Disabled);
    }
}
