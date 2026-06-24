use std::path::Path;
use std::ptr::null;

use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::UI::Shell::ShellExecuteW;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    MB_ICONERROR, MB_ICONINFORMATION, MB_OK, MB_SETFOREGROUND, MessageBoxW, SW_SHOWNORMAL,
};

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
            &format!("Could not open:\n{target}\n\n{error}"),
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
    match code {
        0 => "The operating system is out of memory or resources.".to_string(),
        2 => "The target file was not found.".to_string(),
        3 => "The target path was not found.".to_string(),
        5 => "Access was denied.".to_string(),
        8 => "There is not enough memory to complete the operation.".to_string(),
        26 => "A sharing violation occurred.".to_string(),
        27 => "The file association is incomplete or invalid.".to_string(),
        28 => "The DDE transaction timed out.".to_string(),
        29 => "The DDE transaction failed.".to_string(),
        30 => "The DDE transaction is busy.".to_string(),
        31 => "No application is associated with the target.".to_string(),
        32 => "The dynamic-link library was not found.".to_string(),
        other => format!("ShellExecuteW returned error code {other}."),
    }
}

pub fn wide_null(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}
