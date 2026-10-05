//! Refuse to act while UAC / credential prompts or the secure desktop are up.

use windows::Win32::Foundation::{HANDLE, HWND};
use windows::Win32::System::StationsAndDesktops::{
    CloseDesktop, GetUserObjectInformationW, OpenInputDesktop, DESKTOP_CONTROL_FLAGS, DESKTOP_READOBJECTS, UOI_NAME,
};
use windows::Win32::UI::WindowsAndMessaging::{GetAncestor, GetForegroundWindow, GA_ROOTOWNER};

use super::wide_to_string;
use super::window::{process_name, window_class, window_pid, window_title};
use crate::{sensitive_reason, Error, Result};

pub fn security_check() -> Result<()> {
    input_desktop_check()?;
    let fg = unsafe { GetForegroundWindow() };
    if !fg.0.is_null() {
        check_window(fg)?;
    }
    Ok(())
}

/// The interactive desktop must be the normal `Default` desktop (not `Winlogon`, used by UAC and the lock screen).
fn input_desktop_check() -> Result<()> {
    unsafe {
        let desk = match OpenInputDesktop(DESKTOP_CONTROL_FLAGS(0), false, DESKTOP_READOBJECTS) {
            Ok(d) => d,
            Err(e) => {
                return Err(Error::SecureDesktop(format!(
                    "the input desktop is not accessible ({}); a UAC prompt or the lock screen is probably showing",
                    e.message().trim()
                )))
            }
        };
        let mut buf = [0u16; 128];
        let mut needed = 0u32;
        let res = GetUserObjectInformationW(
            HANDLE(desk.0),
            UOI_NAME,
            Some(buf.as_mut_ptr() as *mut _),
            (buf.len() * 2) as u32,
            Some(&mut needed),
        );
        let _ = CloseDesktop(desk);
        let name = wide_to_string(&buf);
        if res.is_ok() && !name.is_empty() && !name.eq_ignore_ascii_case("Default") {
            return Err(Error::SecureDesktop(format!(
                "the \"{name}\" desktop is active (UAC prompt, lock screen or Ctrl+Alt+Del screen)"
            )));
        }
    }
    Ok(())
}

/// `Err(SecureDesktop)` when the window is a UAC / credential / sign-in prompt.
pub(crate) fn check_window(h: HWND) -> Result<()> {
    let process = process_name(window_pid(h));
    if let Some(reason) = sensitive_reason(&process, &window_class(h), &window_title(h)) {
        return Err(Error::SecureDesktop(reason));
    }
    Ok(())
}

/// Check a window the agent wants to act on, and the window that owns it.
pub(crate) fn check_target(h: HWND) -> Result<()> {
    check_window(h)?;
    let owner = unsafe { GetAncestor(h, GA_ROOTOWNER) };
    if !owner.0.is_null() && owner != h {
        check_window(owner)?;
    }
    Ok(())
}
