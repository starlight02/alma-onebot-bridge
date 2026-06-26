use std::path::Path;
use std::ptr::null;

use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::UI::Shell::ShellExecuteW;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    MB_ICONERROR, MB_ICONINFORMATION, MB_OK, MB_SETFOREGROUND, MessageBoxW, SW_SHOWNORMAL,
};

use crate::i18n;

pub fn open_path(path: &Path) {
    let target = path.to_string_lossy();
    open_target(&target);
}

pub fn open_url(url: &str) {
    open_target(url);
}

pub fn show_error(title: &str, message: &str) {
    show_message(title, message, MB_OK | MB_ICONERROR | MB_SETFOREGROUND);
}

pub fn show_info(title: &str, message: &str) {
    show_message(
        title,
        message,
        MB_OK | MB_ICONINFORMATION | MB_SETFOREGROUND,
    );
}

fn open_target(target: &str) {
    if let Err(error) = shell_execute(target) {
        tracing::warn!(target = target, error = %error, "ShellExecuteW failed");
        show_error(
            "Alma OneBot Bridge",
            &i18n::open_target_failed(target, &error),
        );
    }
}

fn shell_execute(target: &str) -> Result<(), String> {
    let operation = wide_null("open");
    let target = wide_null(target);
    let result = unsafe {
        ShellExecuteW(
            0 as HWND,
            operation.as_ptr(),
            target.as_ptr(),
            null(),
            null(),
            SW_SHOWNORMAL,
        )
    };

    let code = result as isize;
    if code > 32 {
        Ok(())
    } else {
        Err(shell_execute_error(code))
    }
}

fn show_message(title: &str, message: &str, flags: u32) {
    let title = wide_null(title);
    let message = wide_null(message);
    unsafe {
        MessageBoxW(0 as HWND, message.as_ptr(), title.as_ptr(), flags);
    }
}

fn shell_execute_error(code: isize) -> String {
    i18n::shell_execute_error(code)
}

pub fn wide_null(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}
