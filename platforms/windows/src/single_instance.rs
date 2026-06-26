use std::ptr::null_mut;

use windows_sys::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE};
use windows_sys::Win32::System::Threading::CreateMutexW;

use crate::shell::show_info;
use crate::i18n::{tr, Text};

pub struct SingleInstance {
    handle: HANDLE,
}

impl SingleInstance {
    pub fn acquire(name: &str) -> windows_reactor::Result<Self> {
        let wide = wide_null(name);
        let handle = unsafe { CreateMutexW(null_mut(), 1, wide.as_ptr()) };
        if handle.is_null() {
            return Err(windows_reactor::Error::from_thread());
        }
        if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
            unsafe {
                CloseHandle(handle);
            }
            show_info(
                "Alma OneBot Bridge",
                tr(Text::AppAlreadyRunning),
            );
            std::process::exit(0);
        }
        Ok(Self { handle })
    }
}

impl Drop for SingleInstance {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            unsafe {
                CloseHandle(self.handle);
            }
        }
    }
}

fn wide_null(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}
