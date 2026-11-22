use std::{
    env,
    ffi::{OsStr, c_void},
    fs, io,
    path::{Component, Path, PathBuf},
    process::{self, Command},
    ptr,
    sync::mpsc,
    thread,
    time::{SystemTime, UNIX_EPOCH},
};

use ::windows::{
    Win32::{
        Foundation::{CloseHandle, ERROR_INVALID_PARAMETER, HWND, WAIT_OBJECT_0, WAIT_TIMEOUT},
        Networking::WinHttp::*,
        System::Threading::{OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject},
    },
    core::{Error, HSTRING, PCWSTR, w},
};
use semver::Version;
use serde::Deserialize;
use sha2::{Digest, Sha256};

const REPOSITORY: &str = "Fracizz/windows-monitor-x";
const EXECUTABLE: &str = "windows-monitor-x.exe";
const MAX_RELEASE_JSON: usize = 1024 * 1024;
const MAX_EXECUTABLE: usize = 32 * 1024 * 1024;
const STAGING_PREFIX: &str = ".windows-monitor-update-";
const HELPER: &str = "apply.exe";
const STAGED: &str = "next.exe";
const BACKUP: &str = "previous.exe";
const APPLY_ARG: &str = "--apply-update";
const CLEANUP_ARG: &str = "--cleanup-update";

type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

pub enum Action {
    Check,
    Download(Release),
}

pub enum Event {
    Checked(Result<Option<Release>>),
    Downloaded(Result<StagedUpdate>),
}

pub struct Release {
    pub version: Version,
    path: String,
    size: usize,
    digest: String,
}

#[derive(Deserialize)]
struct GitHubRelease {
    tag_name: String,
    assets: Vec<GitHubAsset>,
}

#[derive(Deserialize)]
struct GitHubAsset {
    name: String,
    size: usize,
    digest: Option<String>,
}

pub fn start(hwnd: HWND, action: Action) -> io::Result<mpsc::Receiver<Event>> {
    let handle = hwnd.0 as usize;
    let (sender, receiver) = mpsc::channel();
    thread::Builder::new()
        .name("monitor-updater".into())
        .spawn(move || {
            let event = match action {
                Action::Check => Event::Checked(check()),
                Action::Download(release) => Event::Downloaded(download(release)),
            };
            if sender.send(event).is_ok() {
                super::notify(HWND(handle as *mut _), super::UPDATE_READY);
            }
        })?;
    Ok(receiver)
}

fn check() -> Result<Option<Release>> {
    let current = Version::parse(env!("CARGO_PKG_VERSION"))?;
    let path = format!("/repos/{REPOSITORY}/releases/latest");
    let json = get(
        w!("api.github.com"),
        &path,
        "Accept: application/vnd.github+json\r\n",
        MAX_RELEASE_JSON,
    )?;
    release_from_json(&json, &current)
}

fn release_from_json(json: &[u8], current: &Version) -> Result<Option<Release>> {
    let release: GitHubRelease = serde_json::from_slice(json)?;
    let tag = &release.tag_name;
    let version = Version::parse(tag.strip_prefix('v').unwrap_or(tag))?;
    if version.cmp_precedence(current).is_le() {
        return Ok(None);
    }
    let asset = release
        .assets
        .into_iter()
        .find(|asset| asset.name == EXECUTABLE)
        .ok_or_else(|| io::Error::other("新版缺少 Windows 可执行文件。"))?;
    let digest = asset
        .digest
        .as_deref()
        .and_then(|digest| digest.strip_prefix("sha256:"))
        .filter(|digest| digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .ok_or_else(|| io::Error::other("GitHub 没有提供有效的 SHA-256 校验值。"))?;
    if !(1..=MAX_EXECUTABLE).contains(&asset.size) {
        return Err(io::Error::other("新版文件大小无效。").into());
    }
    Ok(Some(Release {
        version,
        path: format!("/{REPOSITORY}/releases/download/{tag}/{EXECUTABLE}"),
        size: asset.size,
        digest: digest.to_ascii_lowercase(),
    }))
}

fn download(release: Release) -> Result<StagedUpdate> {
    let bytes = get(w!("github.com"), &release.path, "", release.size)?;
    verify_download(&release, &bytes)?;

    let target = env::current_exe()?;
    let parent = target
        .parent()
        .ok_or_else(|| io::Error::other("无法确定程序目录。"))?;
    let time = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let directory = parent.join(format!("{STAGING_PREFIX}{}-{time}", process::id()));
    fs::create_dir(&directory)?;
    let update = StagedUpdate {
        target,
        directory,
        handed_off: false,
    };
    fs::write(update.directory.join(STAGED), bytes)?;
    fs::copy(&update.target, update.directory.join(HELPER))?;
    Ok(update)
}

fn verify_download(release: &Release, bytes: &[u8]) -> Result<()> {
    if bytes.len() != release.size || format!("{:x}", Sha256::digest(bytes)) != release.digest {
        return Err(io::Error::other("下载文件校验失败，请重新检查更新。").into());
    }
    Ok(())
}

struct HttpHandle(*mut c_void);

impl HttpHandle {
    fn new(handle: *mut c_void) -> Result<Self> {
        if handle.is_null() {
            Err(Error::from_thread().into())
        } else {
            Ok(Self(handle))
        }
    }
}

impl Drop for HttpHandle {
    fn drop(&mut self) {
        let _ = unsafe { WinHttpCloseHandle(self.0) };
    }
}

fn get(host: PCWSTR, path: &str, headers: &str, limit: usize) -> Result<Vec<u8>> {
    let path = HSTRING::from(path);
    let agent = HSTRING::from(concat!("windows-monitor-x/", env!("CARGO_PKG_VERSION")));
    let headers: Vec<_> = headers.encode_utf16().collect();
    let mut bytes = Vec::new();
    unsafe {
        let session = HttpHandle::new(WinHttpOpen(
            &agent,
            WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
            PCWSTR::null(),
            PCWSTR::null(),
            0,
        ))?;
        WinHttpSetTimeouts(session.0, 8_000, 8_000, 8_000, 15_000)?;
        let connection = HttpHandle::new(WinHttpConnect(session.0, host, 443, 0))?;
        let request = HttpHandle::new(WinHttpOpenRequest(
            connection.0,
            w!("GET"),
            &path,
            PCWSTR::null(),
            PCWSTR::null(),
            ptr::null(),
            WINHTTP_FLAG_SECURE,
        ))?;
        WinHttpSendRequest(
            request.0,
            (!headers.is_empty()).then_some(headers.as_slice()),
            None,
            0,
            0,
            0,
        )?;
        WinHttpReceiveResponse(request.0, ptr::null_mut())?;
        let mut status = 0u32;
        let mut size = size_of::<u32>() as u32;
        WinHttpQueryHeaders(
            request.0,
            WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
            PCWSTR::null(),
            Some((&mut status as *mut u32).cast()),
            &mut size,
            ptr::null_mut(),
        )?;
        if status != 200 {
            return Err(io::Error::other(format!("GitHub 返回 HTTP {status}。")).into());
        }
        let mut buffer = [0u8; 16 * 1024];
        loop {
            let mut read = 0;
            WinHttpReadData(
                request.0,
                buffer.as_mut_ptr().cast(),
                buffer.len() as u32,
                &mut read,
            )?;
            if read == 0 {
                return Ok(bytes);
            }
            if bytes.len() + read as usize > limit {
                return Err(io::Error::other("GitHub 响应超过文件大小限制。").into());
            }
            bytes.extend_from_slice(&buffer[..read as usize]);
        }
    }
}

pub struct StagedUpdate {
    target: PathBuf,
    directory: PathBuf,
    handed_off: bool,
}

impl StagedUpdate {
    pub fn launch(mut self) -> Result<()> {
        let filename = self
            .target
            .file_name()
            .ok_or_else(|| io::Error::other("无法确定程序文件名。"))?;
        Command::new(self.directory.join(HELPER))
            .arg(APPLY_ARG)
            .arg(process::id().to_string())
            .arg(filename)
            .spawn()?;
        self.handed_off = true;
        Ok(())
    }
}

impl Drop for StagedUpdate {
    fn drop(&mut self) {
        if !self.handed_off {
            let _ = fs::remove_dir_all(&self.directory);
        }
    }
}

pub fn handle_startup_args() -> Option<Result<()>> {
    let mut args = env::args_os().skip(1);
    let action = args.next()?;
    if action == APPLY_ARG {
        return Some(apply(&args.next()?, &args.next()?));
    }
    if action == CLEANUP_ARG
        && let (Some(helper), Some(directory)) = (args.next(), args.next())
    {
        cleanup(&helper, &directory);
    }
    None
}

fn apply(parent: &OsStr, filename: &OsStr) -> Result<()> {
    let helper = env::current_exe()?;
    let directory = helper.parent().filter(|path| {
        helper.file_name() == Some(OsStr::new(HELPER))
            && path.file_name().is_some_and(is_staging_name)
    });
    let directory = directory.ok_or_else(|| io::Error::other("更新目录无效。"))?;
    if !is_filename(filename)
        || !Path::new(filename)
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("exe"))
    {
        return Err(io::Error::other("更新目标无效。").into());
    }
    let target = directory
        .parent()
        .ok_or_else(|| io::Error::other("更新目标无效。"))?
        .join(filename);
    wait_for_exit(parent)?;

    if let Err(error) = replace_and_restart(&target, directory) {
        let _ = Command::new(&target).spawn();
        return Err(error);
    }
    Ok(())
}

fn replace_and_restart(target: &Path, directory: &Path) -> Result<()> {
    let backup = directory.join(BACKUP);
    fs::copy(target, &backup)?;
    fs::rename(directory.join(STAGED), target)?;
    let result = Command::new(target)
        .arg(CLEANUP_ARG)
        .arg(process::id().to_string())
        .arg(directory.file_name().expect("staging directory has a name"))
        .spawn();
    if let Err(error) = result {
        fs::rename(&backup, target).map_err(|restore| {
            io::Error::other(format!(
                "无法启动新版或恢复旧版。旧版备份位于 {}。{restore}",
                backup.display()
            ))
        })?;
        return Err(error.into());
    }
    Ok(())
}

fn cleanup(helper: &OsStr, directory: &OsStr) {
    if is_staging_name(directory)
        && let Ok(exe) = env::current_exe()
        && let Some(parent) = exe.parent()
        && wait_for_exit(helper).is_ok()
    {
        let _ = fs::remove_dir_all(parent.join(directory));
    }
}

fn is_filename(value: &OsStr) -> bool {
    let mut parts = Path::new(value).components();
    matches!(parts.next(), Some(Component::Normal(name)) if name == value)
        && parts.next().is_none()
        && !value.to_string_lossy().contains(':')
}

fn is_staging_name(value: &OsStr) -> bool {
    value
        .to_str()
        .and_then(|name| name.strip_prefix(STAGING_PREFIX))
        .and_then(|suffix| suffix.split_once('-'))
        .is_some_and(|(pid, time)| pid.parse::<u32>().is_ok() && time.parse::<u128>().is_ok())
}

fn wait_for_exit(pid: &OsStr) -> Result<()> {
    let pid = pid
        .to_str()
        .and_then(|pid| pid.parse::<u32>().ok())
        .filter(|&pid| pid != 0 && pid != process::id())
        .ok_or_else(|| io::Error::other("更新进程编号无效。"))?;
    let handle = match unsafe { OpenProcess(PROCESS_SYNCHRONIZE, false, pid) } {
        Ok(handle) => handle,
        Err(error) if error.code() == ERROR_INVALID_PARAMETER.to_hresult() => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    let result = match unsafe { WaitForSingleObject(handle, 15_000) } {
        WAIT_OBJECT_0 => Ok(()),
        WAIT_TIMEOUT => Err(io::Error::new(io::ErrorKind::TimedOut, "等待旧进程关闭超时。").into()),
        _ => Err(Error::from_thread().into()),
    };
    let _ = unsafe { CloseHandle(handle) };
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selects_newer_github_executable_and_checks_download() {
        let bytes = b"test executable";
        let json = serde_json::json!({
            "tag_name": "v0.4.0",
            "assets": [
                { "name": "source.zip", "size": 1 },
                {
                    "name": EXECUTABLE,
                    "size": bytes.len(),
                    "digest": format!("sha256:{:x}", Sha256::digest(bytes)),
                }
            ]
        });
        let payload = json.to_string();
        let release = release_from_json(payload.as_bytes(), &Version::new(0, 3, 1))
            .unwrap()
            .unwrap();
        assert_eq!(release.version, Version::new(0, 4, 0));
        assert_eq!(
            release.path,
            format!("/{REPOSITORY}/releases/download/v0.4.0/{EXECUTABLE}")
        );
        assert!(verify_download(&release, bytes).is_ok());
        assert!(verify_download(&release, b"bad! executable").is_err());
        assert!(
            release_from_json(payload.as_bytes(), &Version::new(0, 4, 0))
                .unwrap()
                .is_none()
        );
        assert!(
            release_from_json(payload.as_bytes(), &Version::new(0, 5, 0))
                .unwrap()
                .is_none()
        );

        let mut malformed = json;
        malformed["assets"][1]["digest"] = serde_json::Value::Null;
        assert!(
            release_from_json(malformed.to_string().as_bytes(), &Version::new(0, 3, 1)).is_err()
        );
    }
}
