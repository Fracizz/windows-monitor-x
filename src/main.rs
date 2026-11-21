#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(any(windows, test))]
mod model;
#[cfg(windows)]
mod windows;

#[cfg(windows)]
fn main() {
    if let Err(error) = windows::run() {
        windows::show_error(&error);
        std::process::exit(1);
    }
}

#[cfg(not(windows))]
fn main() {
    eprintln!("Windows Monitor runs on Windows 10 or later.");
}
