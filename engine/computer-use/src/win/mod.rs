//! Windows backend: Win32 window management, GDI capture, SendInput and UI Automation.

mod capture;
mod clipboard;
mod input;
mod security;
mod uia;
mod window;

pub use capture::screenshot;
pub use clipboard::{clipboard_get, clipboard_set};
pub use input::{keyboard_keys, keyboard_type, mouse, post_click};
pub use security::security_check;
pub use uia::{find_elements, ui_action, ui_tree};
pub use window::{
    expect_foreground, foreground_window, init_dpi_awareness, launch, launch_and_wait, list_windows, monitors,
    wait_for_window, window_at, window_info, window_op,
};

use windows::Win32::Foundation::{HWND, RECT};

use crate::{Error, Rect};

impl From<windows::core::Error> for Error {
    fn from(e: windows::core::Error) -> Self {
        Error::Win(format!("{} (HRESULT 0x{:08X})", e.message().trim(), e.code().0 as u32))
    }
}

/// Attach a short context to a Win32 error.
pub(crate) fn win_err(context: &str, e: windows::core::Error) -> Error {
    Error::Win(format!("{context}: {} (HRESULT 0x{:08X})", e.message().trim(), e.code().0 as u32))
}

pub(crate) fn last_error(context: &str) -> Error {
    win_err(context, windows::core::Error::from_win32())
}

pub(crate) fn to_hwnd(h: isize) -> HWND {
    HWND(h as *mut core::ffi::c_void)
}

pub(crate) fn from_hwnd(h: HWND) -> isize {
    h.0 as isize
}

pub(crate) fn rect_from(r: RECT) -> Rect {
    Rect {
        x: r.left as f64,
        y: r.top as f64,
        width: (r.right - r.left).max(0) as f64,
        height: (r.bottom - r.top).max(0) as f64,
    }
}

pub(crate) fn wide_to_string(buf: &[u16]) -> String {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end])
}
