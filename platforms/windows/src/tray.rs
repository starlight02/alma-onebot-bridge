use std::mem::{size_of, zeroed};
use std::ptr::{null, null_mut};
use std::sync::{Arc, OnceLock};

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Shell::{
    NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NOTIFYICONDATAW, Shell_NotifyIconW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu, GetCursorPos,
    HICON, HMENU, IDI_APPLICATION, LoadIconW, MF_GRAYED, MF_SEPARATOR, MF_STRING, PostQuitMessage,
    RegisterClassW, SetForegroundWindow, TPM_LEFTALIGN, TPM_RIGHTBUTTON, TrackPopupMenu,
    WM_COMMAND, WM_DESTROY, WM_LBUTTONDBLCLK, WM_RBUTTONUP, WM_USER, WNDCLASSW, WS_OVERLAPPED,
};

use crate::app_state::AppState;
use crate::i18n::{tr, Text};
use crate::shell::{open_path, wide_null};
use crate::ui;

const CLASS_NAME: &str = "AlmaOneBotBridgeTrayWindow";
const APP_ICON_RESOURCE_ID: usize = 1;
const WM_TRAYICON: u32 = WM_USER + 1;
const TRAY_UID: u32 = 1;

const IDM_SETTINGS: usize = 1001;
const IDM_START: usize = 1002;
const IDM_STOP: usize = 1003;
const IDM_RESTART: usize = 1004;
const IDM_OPEN_CONFIG: usize = 1005;
const IDM_OPEN_LOG: usize = 1006;
const IDM_ABOUT: usize = 1007;
const IDM_QUIT: usize = 1008;

static APP_STATE: OnceLock<Arc<AppState>> = OnceLock::new();

pub fn install(state: Arc<AppState>) -> windows_reactor::Result<()> {
    let _ = APP_STATE.set(state);
    let hwnd = create_message_window()?;
    add_tray_icon(hwnd)?;
    Ok(())
}

fn create_message_window() -> windows_reactor::Result<HWND> {
    let class_name = wide_null(CLASS_NAME);
    let instance = unsafe { GetModuleHandleW(null()) };
    let wnd_class = WNDCLASSW {
        lpfnWndProc: Some(wnd_proc),
        hInstance: instance,
        hIcon: load_app_icon(instance),
        lpszClassName: class_name.as_ptr(),
        ..unsafe { zeroed() }
    };
    let atom = unsafe { RegisterClassW(&wnd_class) };
    if atom == 0 {
        return Err(windows_reactor::Error::from_thread());
    }

    let hwnd = unsafe {
        CreateWindowExW(
            0,
            class_name.as_ptr(),
            class_name.as_ptr(),
            WS_OVERLAPPED,
            0,
            0,
            0,
            0,
            null_mut(),
            null_mut(),
            instance,
            null_mut(),
        )
    };
    if hwnd.is_null() {
        Err(windows_reactor::Error::from_thread())
    } else {
        Ok(hwnd)
    }
}

fn add_tray_icon(hwnd: HWND) -> windows_reactor::Result<()> {
    let instance = unsafe { GetModuleHandleW(null()) };
    let mut nid: NOTIFYICONDATAW = unsafe { zeroed() };
    nid.cbSize = size_of::<NOTIFYICONDATAW>() as u32;
    nid.hWnd = hwnd;
    nid.uID = TRAY_UID;
    nid.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
    nid.uCallbackMessage = WM_TRAYICON;
    nid.hIcon = load_app_icon(instance);
    write_tip(&mut nid.szTip, "Alma OneBot Bridge");

    let ok = unsafe { Shell_NotifyIconW(NIM_ADD, &mut nid) };
    if ok == 0 {
        Err(windows_reactor::Error::from_thread())
    } else {
        Ok(())
    }
}

fn load_app_icon(instance: *mut std::ffi::c_void) -> HICON {
    let icon = unsafe { LoadIconW(instance, APP_ICON_RESOURCE_ID as *const u16) as HICON };
    if icon.is_null() {
        unsafe { LoadIconW(null_mut(), IDI_APPLICATION) as HICON }
    } else {
        icon
    }
}

fn remove_tray_icon(hwnd: HWND) {
    let mut nid: NOTIFYICONDATAW = unsafe { zeroed() };
    nid.cbSize = size_of::<NOTIFYICONDATAW>() as u32;
    nid.hWnd = hwnd;
    nid.uID = TRAY_UID;
    unsafe {
        Shell_NotifyIconW(NIM_DELETE, &mut nid);
    }
}

unsafe extern "system" fn wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_TRAYICON => {
            match lparam as u32 {
                WM_LBUTTONDBLCLK => show_settings(),
                WM_RBUTTONUP => show_menu(hwnd),
                _ => {}
            }
            0
        }
        WM_COMMAND => {
            handle_command((wparam & 0xffff) as usize);
            0
        }
        WM_DESTROY => {
            remove_tray_icon(hwnd);
            unsafe {
                PostQuitMessage(0);
            }
            0
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

fn show_menu(hwnd: HWND) {
    let Some(state) = APP_STATE.get() else {
        return;
    };
    let snapshot = state.snapshot();
    let status = snapshot.status_line();

    unsafe {
        let menu = CreatePopupMenu();
        if menu.is_null() {
            return;
        }
        append(menu, IDM_SETTINGS, tr(Text::Settings));
        append(menu, IDM_START, tr(Text::StartBridge));
        append(menu, IDM_STOP, tr(Text::StopBridge));
        append(menu, IDM_RESTART, tr(Text::RestartBridge));
        append(menu, IDM_OPEN_CONFIG, tr(Text::OpenConfigDirectory));
        append(menu, IDM_OPEN_LOG, tr(Text::OpenBridgeLog));
        append(menu, IDM_ABOUT, tr(Text::About));
        AppendMenuW(menu, MF_SEPARATOR, 0, null());
        append_disabled(menu, &status);
        AppendMenuW(menu, MF_SEPARATOR, 0, null());
        append(menu, IDM_QUIT, tr(Text::Quit));

        let mut point = POINT { x: 0, y: 0 };
        GetCursorPos(&mut point);
        SetForegroundWindow(hwnd);
        TrackPopupMenu(
            menu,
            TPM_LEFTALIGN | TPM_RIGHTBUTTON,
            point.x,
            point.y,
            0,
            hwnd,
            null(),
        );
        DestroyMenu(menu);
    }
}

unsafe fn append(menu: HMENU, id: usize, text: &str) {
    let text = wide_null(text);
    unsafe {
        AppendMenuW(menu, MF_STRING, id, text.as_ptr());
    }
}

unsafe fn append_disabled(menu: HMENU, text: &str) {
    let text = wide_null(text);
    unsafe {
        AppendMenuW(menu, MF_STRING | MF_GRAYED, 0, text.as_ptr());
    }
}

fn handle_command(id: usize) {
    let Some(state) = APP_STATE.get() else {
        return;
    };
    match id {
        IDM_SETTINGS => show_settings(),
        IDM_START => state.start_bridge(),
        IDM_STOP => state.stop_bridge(),
        IDM_RESTART => state.restart_bridge(),
        IDM_OPEN_CONFIG => open_path(&state.config_dir()),
        IDM_OPEN_LOG => open_path(&state.log_file()),
        IDM_ABOUT => ui::show_about(state.clone()),
        IDM_QUIT => state.quit(),
        _ => {}
    }
}

fn show_settings() {
    if let Some(state) = APP_STATE.get() {
        ui::show_settings(state.clone());
    }
}

fn write_tip(buffer: &mut [u16], text: &str) {
    let wide = text.encode_utf16().collect::<Vec<_>>();
    let len = wide.len().min(buffer.len().saturating_sub(1));
    buffer[..len].copy_from_slice(&wide[..len]);
    buffer[len] = 0;
}
