//! Window enumeration/metadata, DPI awareness, monitors, window operations and app launching.

use std::collections::HashSet;
use std::sync::Once;
use std::time::{Duration, Instant};

use windows::core::{w, HSTRING, PCWSTR, PWSTR};
use windows::Win32::Foundation::{CloseHandle, BOOL, HWND, LPARAM, RECT, TRUE, WPARAM};
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS};
use windows::Win32::Graphics::Gdi::{EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO};
use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE};
use windows::Win32::System::Threading::{
    AttachThreadInput, GetCurrentThreadId, GetProcessId, OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
    PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::HiDpi::{
    GetDpiForMonitor, SetProcessDpiAwarenessContext, SetThreadDpiAwarenessContext,
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, MDT_EFFECTIVE_DPI,
};
use windows::Win32::UI::Shell::{
    ShellExecuteExW, SEE_MASK_FLAG_NO_UI, SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AllowSetForegroundWindow, BringWindowToTop, EnumChildWindows, EnumWindows, GetAncestor, GetClassNameW,
    GetForegroundWindow, GetSystemMetrics, GetWindowLongPtrW, GetWindowRect, GetWindowTextLengthW, GetWindowTextW,
    GetWindowThreadProcessId, IsIconic, IsWindow, IsWindowVisible, IsZoomed, PostMessageW, SetForegroundWindow,
    SetWindowPos, ShowWindow, ASFW_ANY, GA_ROOT, GA_ROOTOWNER, GWL_EXSTYLE, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN,
    SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, SW_MAXIMIZE,
    SW_MINIMIZE, SW_RESTORE, SW_SHOWNORMAL, WM_CLOSE, WS_EX_TOOLWINDOW,
};

use super::{from_hwnd, input, rect_from, security, to_hwnd, win_err};
use crate::{
    app_matches, kill_switch, quote_windows_arg, Error, MonitorInfo, Result, WindowInfo, WindowMatch, WindowOp,
};

static DPI_ONCE: Once = Once::new();

pub fn init_dpi_awareness() {
    DPI_ONCE.call_once(|| unsafe {
        // Fails harmlessly when a manifest or an earlier call already set the awareness.
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    });
    // Per-thread fallback in case the process-wide call was too late (windows already created).
    unsafe {
        let _ = SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
}

unsafe extern "system" fn collect_hwnd(h: HWND, lp: LPARAM) -> BOOL {
    let v = &mut *(lp.0 as *mut Vec<HWND>);
    v.push(h);
    TRUE
}

pub(crate) fn top_level_windows() -> Vec<HWND> {
    let mut v: Vec<HWND> = Vec::new();
    unsafe {
        let _ = EnumWindows(Some(collect_hwnd), LPARAM(&mut v as *mut Vec<HWND> as isize));
    }
    v
}

fn child_windows(parent: HWND) -> Vec<HWND> {
    let mut v: Vec<HWND> = Vec::new();
    unsafe {
        let _ = EnumChildWindows(parent, Some(collect_hwnd), LPARAM(&mut v as *mut Vec<HWND> as isize));
    }
    v
}

pub(crate) fn window_title(h: HWND) -> String {
    unsafe {
        let len = GetWindowTextLengthW(h);
        if len <= 0 {
            return String::new();
        }
        let mut buf = vec![0u16; len as usize + 1];
        let n = GetWindowTextW(h, &mut buf);
        String::from_utf16_lossy(&buf[..n.max(0) as usize])
    }
}

pub(crate) fn window_class(h: HWND) -> String {
    let mut buf = [0u16; 256];
    let n = unsafe { GetClassNameW(h, &mut buf) };
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

pub(crate) fn window_pid(h: HWND) -> u32 {
    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(h, Some(&mut pid)) };
    pid
}

pub(crate) fn process_path(pid: u32) -> Option<String> {
    if pid == 0 {
        return None;
    }
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = vec![0u16; 1024];
        let mut size = buf.len() as u32;
        let res = QueryFullProcessImageNameW(handle, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut size);
        let _ = CloseHandle(handle);
        res.ok()?;
        Some(String::from_utf16_lossy(&buf[..size as usize]))
    }
}

/// Executable file name of a process (`Notepad.exe`), or "" when it can't be queried.
pub(crate) fn process_name(pid: u32) -> String {
    process_path(pid).map(|p| p.rsplit(['\\', '/']).next().unwrap_or(&p).to_string()).unwrap_or_default()
}

fn is_cloaked(h: HWND) -> bool {
    let mut cloaked = 0u32;
    let res = unsafe {
        DwmGetWindowAttribute(h, DWMWA_CLOAKED, &mut cloaked as *mut u32 as *mut _, std::mem::size_of::<u32>() as u32)
    };
    res.is_ok() && cloaked != 0
}

/// The visible frame (DWM extended frame bounds, without the invisible resize borders), physical pixels.
pub(crate) fn frame_rect(h: HWND) -> RECT {
    let mut r = RECT::default();
    let dwm = unsafe {
        DwmGetWindowAttribute(
            h,
            DWMWA_EXTENDED_FRAME_BOUNDS,
            &mut r as *mut RECT as *mut _,
            std::mem::size_of::<RECT>() as u32,
        )
    };
    if dwm.is_err() || r.right <= r.left {
        unsafe {
            let _ = GetWindowRect(h, &mut r);
        }
    }
    r
}

pub(crate) fn window_rect(h: HWND) -> RECT {
    let mut r = RECT::default();
    unsafe {
        let _ = GetWindowRect(h, &mut r);
    }
    r
}

pub(crate) fn require_window(hwnd: isize) -> Result<HWND> {
    let h = to_hwnd(hwnd);
    if hwnd == 0 || !unsafe { IsWindow(h) }.as_bool() {
        return Err(Error::NotFound(format!("window {hwnd} does not exist (list windows again)")));
    }
    Ok(h)
}

/// Executable name, resolving UWP apps hosted by `ApplicationFrameHost.exe` to the real app process.
fn app_for(h: HWND, pid: u32) -> String {
    let name = process_name(pid);
    if name.eq_ignore_ascii_case("ApplicationFrameHost.exe") {
        for child in child_windows(h) {
            let cpid = window_pid(child);
            if cpid != 0 && cpid != pid {
                let n = process_name(cpid);
                if !n.is_empty() {
                    return n;
                }
            }
        }
    }
    name
}

pub(crate) fn info_for(h: HWND, allowed_apps: &[String]) -> WindowInfo {
    let pid = window_pid(h);
    let app = app_for(h, pid);
    let fg = unsafe { GetForegroundWindow() };
    WindowInfo {
        handle: from_hwnd(h).to_string(),
        title: window_title(h),
        allowed: app_matches(&app, allowed_apps),
        app,
        pid,
        bounds: rect_from(frame_rect(h)),
        minimized: unsafe { IsIconic(h) }.as_bool(),
        focused: fg == h,
    }
}

fn listable(h: HWND) -> bool {
    unsafe {
        if !IsWindowVisible(h).as_bool() || is_cloaked(h) {
            return false;
        }
        let ex = GetWindowLongPtrW(h, GWL_EXSTYLE) as u32;
        if ex & WS_EX_TOOLWINDOW.0 != 0 {
            return false;
        }
        GetWindowTextLengthW(h) > 0
    }
}

pub fn list_windows(allowed_apps: &[String]) -> Result<Vec<WindowInfo>> {
    init_dpi_awareness();
    Ok(top_level_windows().into_iter().filter(|&h| listable(h)).map(|h| info_for(h, allowed_apps)).collect())
}

pub fn foreground_window() -> Option<WindowInfo> {
    init_dpi_awareness();
    let fg = unsafe { GetForegroundWindow() };
    if fg.0.is_null() {
        None
    } else {
        Some(info_for(fg, &[]))
    }
}

pub fn window_info(hwnd: isize, allowed_apps: &[String]) -> Result<WindowInfo> {
    init_dpi_awareness();
    let h = require_window(hwnd)?;
    Ok(info_for(h, allowed_apps))
}

pub fn window_at(x: i32, y: i32, allowed_apps: &[String]) -> Result<Option<WindowInfo>> {
    init_dpi_awareness();
    Ok(root_window_at(x, y).map(|h| info_for(h, allowed_apps)))
}

unsafe extern "system" fn collect_monitor(m: HMONITOR, _dc: HDC, _r: *mut RECT, lp: LPARAM) -> BOOL {
    let v = &mut *(lp.0 as *mut Vec<HMONITOR>);
    v.push(m);
    TRUE
}

pub fn monitors() -> Result<Vec<MonitorInfo>> {
    init_dpi_awareness();
    let mut handles: Vec<HMONITOR> = Vec::new();
    unsafe {
        let _ = EnumDisplayMonitors(
            HDC::default(),
            None,
            Some(collect_monitor),
            LPARAM(&mut handles as *mut Vec<HMONITOR> as isize),
        );
    }
    let mut out = Vec::new();
    for m in handles {
        let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        if !unsafe { GetMonitorInfoW(m, &mut mi) }.as_bool() {
            continue;
        }
        let (mut dx, mut dy) = (96u32, 96u32);
        let _ = unsafe { GetDpiForMonitor(m, MDT_EFFECTIVE_DPI, &mut dx, &mut dy) };
        out.push(MonitorInfo {
            index: 0,
            bounds: rect_from(mi.rcMonitor),
            work_area: rect_from(mi.rcWork),
            primary: mi.dwFlags & 1 != 0, // MONITORINFOF_PRIMARY
            scale: dx as f64 / 96.0,
        });
    }
    if out.is_empty() {
        return Err(Error::NotFound("no monitors".into()));
    }
    out.sort_by(|a, b| {
        b.primary.cmp(&a.primary).then(a.bounds.x.total_cmp(&b.bounds.x)).then(a.bounds.y.total_cmp(&b.bounds.y))
    });
    for (i, m) in out.iter_mut().enumerate() {
        m.index = i;
    }
    Ok(out)
}

/// `(x, y, width, height)` of the virtual screen in physical pixels.
pub(crate) fn virtual_screen() -> (i32, i32, i32, i32) {
    unsafe {
        (
            GetSystemMetrics(SM_XVIRTUALSCREEN),
            GetSystemMetrics(SM_YVIRTUALSCREEN),
            GetSystemMetrics(SM_CXVIRTUALSCREEN),
            GetSystemMetrics(SM_CYVIRTUALSCREEN),
        )
    }
}

/// True when `h` is the foreground window or the foreground window belongs to it (e.g. its dialog).
pub(crate) fn is_foreground(h: HWND) -> bool {
    unsafe {
        let fg = GetForegroundWindow();
        if fg.0.is_null() {
            return false;
        }
        fg == h || GetAncestor(fg, GA_ROOT) == h || GetAncestor(fg, GA_ROOTOWNER) == h
    }
}

fn wait_until(timeout: Duration, mut f: impl FnMut() -> bool) -> bool {
    let start = Instant::now();
    loop {
        if f() {
            return true;
        }
        if start.elapsed() >= timeout {
            return false;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// Bring a window to the foreground despite focus-stealing prevention.
pub(crate) fn bring_to_front(h: HWND) -> Result<()> {
    unsafe {
        if IsIconic(h).as_bool() {
            let _ = ShowWindow(h, SW_RESTORE);
        }
        if is_foreground(h) {
            return Ok(());
        }
        let _ = AllowSetForegroundWindow(ASFW_ANY);
        let me = GetCurrentThreadId();
        let fg = GetForegroundWindow();
        let fg_thread = if fg.0.is_null() { 0 } else { GetWindowThreadProcessId(fg, None) };
        let target_thread = GetWindowThreadProcessId(h, None);
        let attach_fg = fg_thread != 0 && fg_thread != me;
        let attach_target = target_thread != 0 && target_thread != me && target_thread != fg_thread;
        // Sharing the input state with the foreground thread lets SetForegroundWindow succeed.
        if attach_fg {
            let _ = AttachThreadInput(me, fg_thread, true);
        }
        if attach_target {
            let _ = AttachThreadInput(me, target_thread, true);
        }
        let _ = BringWindowToTop(h);
        let _ = SetForegroundWindow(h);
        if attach_target {
            let _ = AttachThreadInput(me, target_thread, false);
        }
        if attach_fg {
            let _ = AttachThreadInput(me, fg_thread, false);
        }
    }
    if wait_until(Duration::from_millis(400), || is_foreground(h)) {
        return Ok(());
    }
    // The process that sent the last input event may set the foreground window; an empty relative mouse move
    // makes us that process without visible effect.
    input::nudge();
    unsafe {
        let _ = SetForegroundWindow(h);
    }
    if wait_until(Duration::from_millis(800), || is_foreground(h)) {
        Ok(())
    } else {
        Err(Error::Win(
            "Windows refused to bring the window to the foreground (focus-stealing prevention); ask the user to \
             click it"
                .into(),
        ))
    }
}

pub fn expect_foreground(hwnd: isize) -> Result<()> {
    let h = require_window(hwnd)?;
    if is_foreground(h) {
        return Ok(());
    }
    let fg = unsafe { GetForegroundWindow() };
    let title = if fg.0.is_null() { String::new() } else { window_title(fg) };
    Err(Error::Other(format!("window {hwnd} is not in the foreground (\"{title}\" is); focus it first")))
}

/// Distance from the window rect to the visible frame: (left, top, right, bottom).
fn frame_insets(h: HWND) -> (i32, i32, i32, i32) {
    let wr = window_rect(h);
    let fr = frame_rect(h);
    (fr.left - wr.left, fr.top - wr.top, wr.right - fr.right, wr.bottom - fr.bottom)
}

fn restore_for_geometry(h: HWND) {
    unsafe {
        if IsZoomed(h).as_bool() || IsIconic(h).as_bool() {
            let _ = ShowWindow(h, SW_RESTORE);
        }
    }
}

pub fn window_op(hwnd: isize, op: WindowOp) -> Result<()> {
    init_dpi_awareness();
    let h = require_window(hwnd)?;
    security::check_target(h)?;
    match op {
        WindowOp::Focus => {
            security::security_check()?;
            bring_to_front(h)
        }
        WindowOp::Move { x, y } => {
            restore_for_geometry(h);
            let (dl, dt, _, _) = frame_insets(h);
            unsafe {
                SetWindowPos(h, HWND::default(), x - dl, y - dt, 0, 0, SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE)
            }
            .map_err(|e| win_err("SetWindowPos", e))
        }
        WindowOp::Resize { width, height } => {
            if width <= 0 || height <= 0 {
                return Err(Error::Other(format!("invalid window size {width}x{height}")));
            }
            restore_for_geometry(h);
            let (dl, dt, dr, db) = frame_insets(h);
            unsafe {
                SetWindowPos(
                    h,
                    HWND::default(),
                    0,
                    0,
                    width + dl + dr,
                    height + dt + db,
                    SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
                )
            }
            .map_err(|e| win_err("SetWindowPos", e))
        }
        WindowOp::Minimize => {
            unsafe {
                let _ = ShowWindow(h, SW_MINIMIZE);
            }
            Ok(())
        }
        WindowOp::Maximize => {
            unsafe {
                let _ = ShowWindow(h, SW_MAXIMIZE);
            }
            Ok(())
        }
        WindowOp::Restore => {
            unsafe {
                let _ = ShowWindow(h, SW_RESTORE);
            }
            Ok(())
        }
        WindowOp::Close => {
            unsafe { PostMessageW(h, WM_CLOSE, WPARAM(0), LPARAM(0)) }.map_err(|e| win_err("WM_CLOSE", e))
        }
    }
}

pub fn launch(app: &str, args: &[String]) -> Result<u32> {
    let app = app.trim().to_string();
    if app.is_empty() {
        return Err(Error::Other("no app to launch".into()));
    }
    let params = args.iter().map(|a| quote_windows_arg(a)).collect::<Vec<_>>().join(" ");
    // ShellExecuteEx may load shell extensions, which want an STA thread; use a short-lived one.
    std::thread::spawn(move || unsafe {
        let hr = CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE);
        let file = HSTRING::from(app.as_str());
        let params_w = HSTRING::from(params.as_str());
        let mut info = SHELLEXECUTEINFOW {
            cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
            fMask: SEE_MASK_NOCLOSEPROCESS | SEE_MASK_FLAG_NO_UI | SEE_MASK_NOASYNC,
            lpVerb: w!("open"),
            lpFile: PCWSTR(file.as_ptr()),
            lpParameters: if params.is_empty() { PCWSTR::null() } else { PCWSTR(params_w.as_ptr()) },
            nShow: SW_SHOWNORMAL.0,
            ..Default::default()
        };
        let result = match ShellExecuteExW(&mut info) {
            Ok(()) => {
                let pid = if info.hProcess.is_invalid() {
                    0
                } else {
                    let pid = GetProcessId(info.hProcess);
                    let _ = CloseHandle(info.hProcess);
                    pid
                };
                Ok(pid)
            }
            Err(e) => Err(win_err(&format!("could not launch `{app}`"), e)),
        };
        if hr.is_ok() {
            CoUninitialize();
        }
        result
    })
    .join()
    .map_err(|_| Error::Other("launch thread panicked".into()))?
}

fn window_matches(w: &WindowInfo, target: &WindowMatch) -> bool {
    match target {
        WindowMatch::Pid(pid) => w.pid == *pid,
        WindowMatch::App(app) => app_matches(&w.app, std::slice::from_ref(app)),
    }
}

pub fn wait_for_window(target: &WindowMatch, timeout: Duration) -> Result<WindowInfo> {
    let deadline = Instant::now() + timeout;
    loop {
        kill_switch::check()?;
        if let Some(w) = list_windows(&[])?.into_iter().find(|w| window_matches(w, target)) {
            return Ok(w);
        }
        if Instant::now() >= deadline {
            return Err(Error::NotFound(format!("no window for {target:?} within {timeout:?}")));
        }
        std::thread::sleep(Duration::from_millis(150));
    }
}

pub fn launch_and_wait(app: &str, args: &[String], timeout: Duration) -> Result<WindowInfo> {
    let before: HashSet<String> = list_windows(&[])?.into_iter().map(|w| w.handle).collect();
    let pid = launch(app, args)?;
    let file = app.trim().rsplit(['\\', '/']).next().unwrap_or(app).to_string();
    let stem = file.strip_suffix(".exe").or_else(|| file.strip_suffix(".EXE")).unwrap_or(&file).to_string();
    let deadline = Instant::now() + timeout;
    loop {
        kill_switch::check()?;
        let new_window = list_windows(&[])?.into_iter().filter(|w| !before.contains(&w.handle)).find(|w| {
            (pid != 0 && w.pid == pid) || (!stem.is_empty() && app_matches(&w.app, std::slice::from_ref(&stem)))
        });
        if let Some(w) = new_window {
            return Ok(w);
        }
        if Instant::now() >= deadline {
            return Err(Error::NotFound(format!("no new window appeared for `{app}` (pid {pid}) within {timeout:?}")));
        }
        std::thread::sleep(Duration::from_millis(150));
    }
}

/// Root window under a physical screen point.
pub(crate) fn root_window_at(x: i32, y: i32) -> Option<HWND> {
    use windows::Win32::Foundation::POINT;
    use windows::Win32::UI::WindowsAndMessaging::WindowFromPoint;
    unsafe {
        let h = WindowFromPoint(POINT { x, y });
        if h.0.is_null() {
            return None;
        }
        let root = GetAncestor(h, GA_ROOT);
        Some(if root.0.is_null() { h } else { root })
    }
}
