//! Unicode text clipboard.

use std::time::Duration;

use windows::Win32::Foundation::{GlobalFree, HANDLE, HGLOBAL, HWND};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, IsClipboardFormatAvailable, OpenClipboard, SetClipboardData,
};
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock, GMEM_MOVEABLE};

use super::{last_error, win_err};
use crate::{Error, Result};

const CF_UNICODETEXT: u32 = 13;

/// Closes the clipboard on drop.
struct Open;

impl Open {
    fn new() -> Result<Self> {
        // Another app may hold the clipboard briefly; retry for ~0.5 s.
        for _ in 0..20 {
            if unsafe { OpenClipboard(HWND::default()) }.is_ok() {
                return Ok(Open);
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        Err(last_error("the clipboard is busy"))
    }
}

impl Drop for Open {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseClipboard();
        }
    }
}

pub fn clipboard_get() -> Result<String> {
    let _open = Open::new()?;
    unsafe {
        if IsClipboardFormatAvailable(CF_UNICODETEXT).is_err() {
            return Ok(String::new());
        }
        let handle = match GetClipboardData(CF_UNICODETEXT) {
            Ok(h) if !h.is_invalid() => h,
            _ => return Ok(String::new()),
        };
        let mem = HGLOBAL(handle.0);
        let ptr = GlobalLock(mem) as *const u16;
        if ptr.is_null() {
            return Err(last_error("GlobalLock"));
        }
        let max = GlobalSize(mem) / 2;
        let mut len = 0usize;
        while len < max && *ptr.add(len) != 0 {
            len += 1;
        }
        let text = String::from_utf16_lossy(std::slice::from_raw_parts(ptr, len));
        let _ = GlobalUnlock(mem);
        Ok(text)
    }
}

pub fn clipboard_set(text: &str) -> Result<()> {
    let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    let _open = Open::new()?;
    unsafe {
        EmptyClipboard().map_err(|e| win_err("EmptyClipboard", e))?;
        let mem = GlobalAlloc(GMEM_MOVEABLE, wide.len() * 2).map_err(|e| win_err("GlobalAlloc", e))?;
        let ptr = GlobalLock(mem) as *mut u16;
        if ptr.is_null() {
            let _ = GlobalFree(mem);
            return Err(Error::Win("GlobalLock failed".into()));
        }
        std::ptr::copy_nonoverlapping(wide.as_ptr(), ptr, wide.len());
        let _ = GlobalUnlock(mem);
        // On success the system owns the memory.
        if let Err(e) = SetClipboardData(CF_UNICODETEXT, HANDLE(mem.0)) {
            let _ = GlobalFree(mem);
            return Err(win_err("SetClipboardData", e));
        }
    }
    Ok(())
}
