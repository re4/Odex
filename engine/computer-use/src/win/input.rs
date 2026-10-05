//! Real mouse and keyboard input via `SendInput`, plus the `PostMessage` background click.

use std::time::Duration;

use windows::Win32::Foundation::{HWND, LPARAM, POINT, WPARAM};
use windows::Win32::Graphics::Gdi::ScreenToClient;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyboardLayout, MapVirtualKeyW, SendInput, VkKeyScanExW, HKL, INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE,
    KEYBDINPUT, KEYBD_EVENT_FLAGS, KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, KEYEVENTF_UNICODE, MAPVK_VK_TO_VSC,
    MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_HWHEEL, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MIDDLEDOWN,
    MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_MOVE, MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_VIRTUALDESK,
    MOUSEEVENTF_WHEEL, MOUSEINPUT, MOUSE_EVENT_FLAGS, VIRTUAL_KEY,
};
use windows::Win32::UI::WindowsAndMessaging::{
    ChildWindowFromPointEx, GetCursorPos, GetForegroundWindow, GetGUIThreadInfo, GetWindowLongPtrW,
    GetWindowThreadProcessId, PostMessageW, SendMessageTimeoutW, SetCursorPos, CWP_SKIPINVISIBLE, CWP_SKIPTRANSPARENT,
    GUITHREADINFO, GWL_STYLE, SMTO_ABORTIFHUNG, SMTO_NORMAL, WM_LBUTTONDBLCLK, WM_LBUTTONDOWN, WM_LBUTTONUP,
    WM_MBUTTONDBLCLK, WM_MBUTTONDOWN, WM_MBUTTONUP, WM_MOUSEMOVE, WM_NULL, WM_RBUTTONDBLCLK, WM_RBUTTONDOWN,
    WM_RBUTTONUP,
};

use super::window::{init_dpi_awareness, require_window, root_window_at, virtual_screen, window_class};
use super::{last_error, security, uia, win_err};
use crate::geometry::to_absolute;
use crate::{kill_switch, Error, Key, KeyCombo, Modifier, MouseAction, MouseButton, NamedKey, Result};

/// Most wheel notches sent per call.
const MAX_NOTCHES: i32 = 50;
const WHEEL_DELTA: i32 = 120;

fn send(inputs: &[INPUT]) -> Result<()> {
    if inputs.is_empty() {
        return Ok(());
    }
    let sent = unsafe { SendInput(inputs, std::mem::size_of::<INPUT>() as i32) };
    if sent as usize != inputs.len() {
        return Err(last_error("SendInput was blocked (the target may be elevated, or another desktop is active)"));
    }
    Ok(())
}

fn mouse_input(dx: i32, dy: i32, data: i32, flags: MOUSE_EVENT_FLAGS) -> INPUT {
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT { dx, dy, mouseData: data as u32, dwFlags: flags, time: 0, dwExtraInfo: 0 },
        },
    }
}

fn key_input(vk: u16, scan: u16, flags: KEYBD_EVENT_FLAGS) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT { wVk: VIRTUAL_KEY(vk), wScan: scan, dwFlags: flags, time: 0, dwExtraInfo: 0 },
        },
    }
}

/// A zero-length relative mouse move: invisible, but makes this process the source of the last input event.
pub(crate) fn nudge() {
    let _ = send(&[mouse_input(0, 0, 0, MOUSEEVENTF_MOVE)]);
}

fn sleep_ms(ms: u64) {
    std::thread::sleep(Duration::from_millis(ms));
}

fn ensure_on_screen(x: i32, y: i32) -> Result<()> {
    let (vx, vy, vw, vh) = virtual_screen();
    if x < vx || y < vy || x >= vx + vw || y >= vy + vh {
        return Err(Error::Other(format!(
            "point ({x}, {y}) is outside the virtual screen ({vx}, {vy}) {vw}x{vh}; map screenshot coordinates first"
        )));
    }
    Ok(())
}

/// Refuse to click on UAC / credential prompts.
fn check_point(x: i32, y: i32) -> Result<()> {
    match root_window_at(x, y) {
        Some(h) => security::check_window(h),
        None => Ok(()),
    }
}

fn move_cursor(x: i32, y: i32) -> Result<()> {
    let (vx, vy, vw, vh) = virtual_screen();
    let (ax, ay) = to_absolute(x, y, vx, vy, vw, vh);
    send(&[mouse_input(ax, ay, 0, MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK)])?;
    // Absolute coordinates are quantised to 1/65536 of the desktop; snap to the exact pixel when needed.
    let mut p = POINT::default();
    unsafe {
        if GetCursorPos(&mut p).is_ok() && (p.x != x || p.y != y) {
            let _ = SetCursorPos(x, y);
        }
    }
    Ok(())
}

fn button_flags(button: MouseButton) -> (MOUSE_EVENT_FLAGS, MOUSE_EVENT_FLAGS) {
    match button {
        MouseButton::Left => (MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP),
        MouseButton::Right => (MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP),
        MouseButton::Middle => (MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP),
    }
}

pub fn mouse(action: MouseAction, x: i32, y: i32) -> Result<()> {
    init_dpi_awareness();
    security::security_check()?;
    ensure_on_screen(x, y)?;
    check_point(x, y)?;
    match action {
        MouseAction::Move => move_cursor(x, y),
        MouseAction::Click { button, double } => {
            move_cursor(x, y)?;
            sleep_ms(30);
            let (down, up) = button_flags(button);
            send(&[mouse_input(0, 0, 0, down), mouse_input(0, 0, 0, up)])?;
            if double {
                sleep_ms(40);
                send(&[mouse_input(0, 0, 0, down), mouse_input(0, 0, 0, up)])?;
            }
            Ok(())
        }
        MouseAction::Drag { to_x, to_y } => {
            ensure_on_screen(to_x, to_y)?;
            check_point(to_x, to_y)?;
            move_cursor(x, y)?;
            sleep_ms(30);
            let (down, up) = button_flags(MouseButton::Left);
            send(&[mouse_input(0, 0, 0, down)])?;
            sleep_ms(60);
            let steps = 16;
            let mut result = Ok(());
            for i in 1..=steps {
                if let Err(e) = kill_switch::check() {
                    result = Err(e);
                    break;
                }
                let ix = x + (to_x - x) * i / steps;
                let iy = y + (to_y - y) * i / steps;
                if let Err(e) = move_cursor(ix, iy) {
                    result = Err(e);
                    break;
                }
                sleep_ms(12);
            }
            sleep_ms(40);
            // Always release the button, even when interrupted.
            send(&[mouse_input(0, 0, 0, up)])?;
            result
        }
        MouseAction::Scroll { dx, dy } => {
            move_cursor(x, y)?;
            sleep_ms(20);
            // Windows: positive wheel data scrolls up / right.
            for _ in 0..dy.abs().min(MAX_NOTCHES) {
                kill_switch::check()?;
                let data = if dy > 0 { -WHEEL_DELTA } else { WHEEL_DELTA };
                send(&[mouse_input(0, 0, data, MOUSEEVENTF_WHEEL)])?;
                sleep_ms(15);
            }
            for _ in 0..dx.abs().min(MAX_NOTCHES) {
                kill_switch::check()?;
                let data = if dx > 0 { WHEEL_DELTA } else { -WHEEL_DELTA };
                send(&[mouse_input(0, 0, data, MOUSEEVENTF_HWHEEL)])?;
                sleep_ms(15);
            }
            Ok(())
        }
    }
}

pub fn post_click(hwnd: isize, x: i32, y: i32, button: MouseButton, double: bool) -> Result<()> {
    init_dpi_awareness();
    let root = require_window(hwnd)?;
    security::security_check()?;
    security::check_target(root)?;
    // Descend to the deepest visible child under the point.
    let mut target = root;
    for _ in 0..32 {
        let mut pt = POINT { x, y };
        let child = unsafe {
            let _ = ScreenToClient(target, &mut pt);
            ChildWindowFromPointEx(target, pt, CWP_SKIPINVISIBLE | CWP_SKIPTRANSPARENT)
        };
        if child.0.is_null() || child == target {
            break;
        }
        target = child;
    }
    let mut pt = POINT { x, y };
    unsafe {
        let _ = ScreenToClient(target, &mut pt);
    }
    let lp = LPARAM((((pt.y as u16 as u32) << 16) | (pt.x as u16 as u32)) as isize);
    let (down, up, dbl, mk) = match button {
        MouseButton::Left => (WM_LBUTTONDOWN, WM_LBUTTONUP, WM_LBUTTONDBLCLK, 0x0001usize),
        MouseButton::Right => (WM_RBUTTONDOWN, WM_RBUTTONUP, WM_RBUTTONDBLCLK, 0x0002),
        MouseButton::Middle => (WM_MBUTTONDOWN, WM_MBUTTONUP, WM_MBUTTONDBLCLK, 0x0010),
    };
    let post = |msg: u32, wp: usize| {
        unsafe { PostMessageW(target, msg, WPARAM(wp), lp) }.map_err(|e| win_err("PostMessage", e))
    };
    post(WM_MOUSEMOVE, 0)?;
    post(down, mk)?;
    post(up, 0)?;
    if double {
        post(dbl, mk)?;
        post(up, 0)?;
    }
    Ok(())
}

/// Classic Win32 / WinForms edit control with `ES_PASSWORD` focused in the foreground thread.
fn win32_focus_is_password() -> bool {
    const ES_PASSWORD: u32 = 0x0020;
    unsafe {
        let fg = GetForegroundWindow();
        if fg.0.is_null() {
            return false;
        }
        let tid = GetWindowThreadProcessId(fg, None);
        let mut gti = GUITHREADINFO { cbSize: std::mem::size_of::<GUITHREADINFO>() as u32, ..Default::default() };
        if GetGUIThreadInfo(tid, &mut gti).is_err() || gti.hwndFocus.0.is_null() {
            return false;
        }
        let focus: HWND = gti.hwndFocus;
        let style = GetWindowLongPtrW(focus, GWL_STYLE) as u32;
        window_class(focus).to_ascii_lowercase().contains("edit") && style & ES_PASSWORD != 0
    }
}

fn refuse_if_password_focus() -> Result<()> {
    if win32_focus_is_password() {
        return Err(Error::PasswordField);
    }
    match uia::focused_is_password() {
        Ok(true) => Err(Error::PasswordField),
        Ok(false) => Ok(()),
        Err(e) => Err(Error::Other(format!(
            "could not verify that the focused element is not a password field ({e}); refusing to type"
        ))),
    }
}

fn vk_event(vk: u16, up: bool) -> INPUT {
    let scan = unsafe { MapVirtualKeyW(vk as u32, MAPVK_VK_TO_VSC) } as u16;
    let mut flags = KEYBD_EVENT_FLAGS(0);
    if is_extended(vk) {
        flags |= KEYEVENTF_EXTENDEDKEY;
    }
    if up {
        flags |= KEYEVENTF_KEYUP;
    }
    key_input(vk, scan, flags)
}

fn is_extended(vk: u16) -> bool {
    matches!(
        vk,
        0x21..=0x28 // PageUp, PageDown, End, Home, arrows
            | 0x2C // PrintScreen
            | 0x2D | 0x2E // Insert, Delete
            | 0x5B..=0x5D // LWin, RWin, Apps
            | 0x6F // Numpad divide
            | 0x90 // NumLock
            | 0xA3 | 0xA5 // RCtrl, RAlt
            | 0xA6..=0xB7 // browser / media keys
    )
}

/// Keyboard layout of the foreground thread (the one receiving our keystrokes).
fn target_layout() -> HKL {
    unsafe {
        let fg = GetForegroundWindow();
        let tid = if fg.0.is_null() { 0 } else { GetWindowThreadProcessId(fg, None) };
        GetKeyboardLayout(tid)
    }
}

/// Pause after each injected key event before synchronising with the target thread.
const KEY_EVENT_DELAY_MS: u64 = 3;
/// Extra pause after each typed character.
const PACKET_DELAY_MS: u64 = 6;
/// Pause before typing starts: apps are often still busy right after a focus change, click or shortcut.
const TYPE_SETTLE_MS: u64 = 40;

/// Let the target finish what it is doing before new input arrives.
fn settle(target: HWND) {
    sync_with(target);
    sleep_ms(TYPE_SETTLE_MS);
    sync_with(target);
}

/// Window whose UI thread receives our keystrokes.
fn input_target() -> HWND {
    unsafe {
        let fg = GetForegroundWindow();
        if fg.0.is_null() {
            return fg;
        }
        let tid = GetWindowThreadProcessId(fg, None);
        let mut gti = GUITHREADINFO { cbSize: std::mem::size_of::<GUITHREADINFO>() as u32, ..Default::default() };
        if GetGUIThreadInfo(tid, &mut gti).is_ok() && !gti.hwndFocus.0.is_null() {
            gti.hwndFocus
        } else {
            fg
        }
    }
}

/// Wait (briefly) until the target UI thread pumps messages again, i.e. has picked up the input we injected.
fn sync_with(target: HWND) {
    if target.0.is_null() {
        return;
    }
    unsafe {
        let _ = SendMessageTimeoutW(target, WM_NULL, WPARAM(0), LPARAM(0), SMTO_NORMAL | SMTO_ABORTIFHUNG, 250, None);
    }
}

/// Inject key events one at a time. XAML/WinUI apps (e.g. the Windows 11 Notepad) drain all queued input before
/// translating it, so batched events lose Shift and Unicode packets all come out as the last character; pacing
/// each event and synchronising with the target thread keeps them in step.
fn send_keys_paced(events: &[INPUT], target: HWND) -> Result<()> {
    for event in events {
        kill_switch::check()?;
        send(std::slice::from_ref(event))?;
        sleep_ms(KEY_EVENT_DELAY_MS);
        sync_with(target);
    }
    Ok(())
}

pub fn keyboard_type(text: &str) -> Result<()> {
    init_dpi_awareness();
    security::security_check()?;
    refuse_if_password_focus()?;
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    let target = input_target();
    settle(target);
    let mut buf = [0u16; 2];
    for c in normalized.chars() {
        match c {
            // Enter/Tab can move focus (e.g. into the password field of a login form): check again afterwards.
            '\n' | '\t' => {
                let vk = if c == '\n' { 0x0D } else { 0x09 };
                send_keys_paced(&[vk_event(vk, false), vk_event(vk, true)], target)?;
                settle(target);
                refuse_if_password_focus()?;
            }
            // Unicode packets: independent of the keyboard layout, Shift/Caps Lock state, dead keys and IMEs.
            _ => {
                for &unit in c.encode_utf16(&mut buf).iter() {
                    send_keys_paced(
                        &[
                            key_input(0, unit, KEYEVENTF_UNICODE),
                            key_input(0, unit, KEYEVENTF_UNICODE | KEYEVENTF_KEYUP),
                        ],
                        target,
                    )?;
                    sleep_ms(PACKET_DELAY_MS);
                }
            }
        }
    }
    Ok(())
}

fn modifier_vk(m: Modifier) -> u16 {
    match m {
        Modifier::Ctrl | Modifier::Cmd => 0x11,
        Modifier::Alt => 0x12,
        Modifier::Shift => 0x10,
        Modifier::Win => 0x5B,
    }
}

fn named_vk(k: NamedKey) -> u16 {
    match k {
        NamedKey::Enter => 0x0D,
        NamedKey::Tab => 0x09,
        NamedKey::Escape => 0x1B,
        NamedKey::Backspace => 0x08,
        NamedKey::Delete => 0x2E,
        NamedKey::Insert => 0x2D,
        NamedKey::Home => 0x24,
        NamedKey::End => 0x23,
        NamedKey::PageUp => 0x21,
        NamedKey::PageDown => 0x22,
        NamedKey::Left => 0x25,
        NamedKey::Up => 0x26,
        NamedKey::Right => 0x27,
        NamedKey::Down => 0x28,
        NamedKey::Space => 0x20,
        NamedKey::CapsLock => 0x14,
        NamedKey::NumLock => 0x90,
        NamedKey::ScrollLock => 0x91,
        NamedKey::PrintScreen => 0x2C,
        NamedKey::Pause => 0x13,
        NamedKey::ContextMenu => 0x5D,
        NamedKey::VolumeUp => 0xAF,
        NamedKey::VolumeDown => 0xAE,
        NamedKey::VolumeMute => 0xAD,
        NamedKey::MediaPlayPause => 0xB3,
        NamedKey::MediaNext => 0xB0,
        NamedKey::MediaPrev => 0xB1,
        NamedKey::BrowserBack => 0xA6,
        NamedKey::BrowserForward => 0xA7,
    }
}

/// Key-down/key-up events for one combo.
fn combo_events(combo: &KeyCombo) -> Result<Vec<INPUT>> {
    let mut mods: Vec<u16> = Vec::new();
    for m in &combo.modifiers {
        let vk = modifier_vk(*m);
        if !mods.contains(&vk) {
            mods.push(vk);
        }
    }
    let mut key_events: Vec<INPUT> = Vec::new();
    match combo.key {
        None => {}
        Some(Key::Function(n)) => {
            let vk = 0x70 + (n as u16 - 1);
            key_events.extend([vk_event(vk, false), vk_event(vk, true)]);
        }
        Some(Key::Named(k)) => {
            let vk = named_vk(k);
            key_events.extend([vk_event(vk, false), vk_event(vk, true)]);
        }
        Some(Key::Char(c)) => {
            let vk = if c.is_ascii_alphanumeric() {
                Some(c.to_ascii_uppercase() as u16)
            } else if (c as u32) <= 0xFFFF {
                let r = unsafe { VkKeyScanExW(c as u16, target_layout()) };
                if r == -1 {
                    None
                } else {
                    // High byte: shift state needed on the current layout (1 shift, 2 ctrl, 4 alt).
                    let shift_state = ((r as u16) >> 8) & 0xFF;
                    for (bit, vk) in [(1u16, 0x10u16), (2, 0x11), (4, 0x12)] {
                        if shift_state & bit != 0 && !mods.contains(&vk) {
                            mods.push(vk);
                        }
                    }
                    Some((r as u16) & 0xFF)
                }
            } else {
                None
            };
            match vk {
                Some(vk) => key_events.extend([vk_event(vk, false), vk_event(vk, true)]),
                None if mods.is_empty() => {
                    let mut buf = [0u16; 2];
                    for unit in c.encode_utf16(&mut buf).iter() {
                        key_events.push(key_input(0, *unit, KEYEVENTF_UNICODE));
                        key_events.push(key_input(0, *unit, KEYEVENTF_UNICODE | KEYEVENTF_KEYUP));
                    }
                }
                None => return Err(Error::Other(format!("key `{c}` is not on the current keyboard layout"))),
            }
        }
    }
    let mut events: Vec<INPUT> = mods.iter().map(|&vk| vk_event(vk, false)).collect();
    events.extend(key_events);
    events.extend(mods.iter().rev().map(|&vk| vk_event(vk, true)));
    Ok(events)
}

pub fn keyboard_keys(combos: &[KeyCombo]) -> Result<()> {
    init_dpi_awareness();
    security::security_check()?;
    let all: Vec<Vec<INPUT>> = combos.iter().map(combo_events).collect::<Result<_>>()?;
    let target = input_target();
    settle(target);
    for (i, (combo, events)) in combos.iter().zip(&all).enumerate() {
        if i > 0 {
            settle(target);
        }
        // Earlier combos may have moved focus, so check before every text-entering combo.
        if combo.enters_text() {
            refuse_if_password_focus()?;
        }
        send_keys_paced(events, target)?;
    }
    Ok(())
}
