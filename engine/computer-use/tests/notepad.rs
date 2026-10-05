//! Windows GUI harness for computer use: drives a real Notepad window plus a tiny WinForms password box.
//!
//! These tests need an interactive desktop and briefly take focus, so they are ignored by default. Run them with
//! `cargo test -p odex-computer-use -- --ignored --test-threads=1`. They only act on windows they launch and close
//! them without saving (cleanup also runs on panic).
#![cfg(windows)]

use std::collections::HashSet;
use std::os::windows::process::CommandExt;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use odex_computer_use::*;

const HELLO: &str = "hello from odex";
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

fn handle(w: &WindowInfo) -> isize {
    w.handle.parse().expect("numeric window handle")
}

fn wait_for(timeout: Duration, mut f: impl FnMut() -> bool) -> bool {
    let start = Instant::now();
    loop {
        if f() {
            return true;
        }
        if start.elapsed() > timeout {
            return false;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn exists(hwnd: isize) -> bool {
    window_info(hwnd, &[]).is_ok()
}

/// The Notepad text area, found with the `ui_tree` filter.
fn document(hwnd: isize) -> Option<UiNode> {
    let tree = ui_tree(hwnd, 40, Some("document"), 4_000).ok()?;
    tree.nodes.into_iter().find(|n| n.role == "Document")
}

fn document_value(hwnd: isize) -> Option<String> {
    document(hwnd).and_then(|d| d.value)
}

fn find_role(hwnd: isize, query: &str, role: &str) -> Option<UiNode> {
    find_elements(hwnd, query).ok()?.into_iter().find(|n| n.role == role && n.enabled)
}

/// Close our Notepad window without saving, using only UI Automation (no keystrokes that could reach another
/// app): File > Close tab, then "Don't save" on the prompt. Returns whether the prompt was answered.
fn close_without_saving(hwnd: isize) -> std::result::Result<bool, String> {
    let mut requested = false;
    if let Some(file) = find_role(hwnd, "file", "MenuItem") {
        if ui_action(hwnd, &file.id, UiActionKind::Expand).is_ok()
            && wait_for(Duration::from_secs(3), || find_role(hwnd, "close tab", "MenuItem").is_some())
        {
            if let Some(item) = find_role(hwnd, "close tab", "MenuItem") {
                requested = ui_action(hwnd, &item.id, UiActionKind::Invoke).is_ok();
            }
        }
    }
    if !requested {
        window_op(hwnd, WindowOp::Close).map_err(|e| e.to_string())?;
    }
    let mut answered = false;
    let mut sent_close = !requested;
    let start = Instant::now();
    let closed = wait_for(Duration::from_secs(15), || {
        if !exists(hwnd) {
            return true;
        }
        if !answered {
            if let Some(b) = find_role(hwnd, "don't save", "Button") {
                answered = ui_action(hwnd, &b.id, UiActionKind::Invoke).is_ok();
            } else if !sent_close && start.elapsed() > Duration::from_secs(5) {
                // The menu route didn't close anything; ask the window itself.
                sent_close = window_op(hwnd, WindowOp::Close).is_ok();
            }
        }
        false
    });
    if closed {
        Ok(answered)
    } else {
        Err(format!("Notepad window {hwnd} did not close"))
    }
}

/// Topmost WinForms window (magenta background) with a password text box, hosted by a PowerShell child.
fn spawn_password_helper(cover: &Rect) -> (Child, isize) {
    let title = format!("Odex harness {}", std::process::id());
    let script = [
        "Add-Type -AssemblyName System.Windows.Forms".to_string(),
        "Add-Type -AssemblyName System.Drawing".to_string(),
        "$f = New-Object System.Windows.Forms.Form".to_string(),
        format!("$f.Text = '{title}'"),
        "$f.StartPosition = 'Manual'".to_string(),
        format!("$f.Location = New-Object System.Drawing.Point({}, {})", cover.x as i32, cover.y as i32),
        format!("$f.Size = New-Object System.Drawing.Size({}, {})", cover.width as i32, cover.height as i32),
        "$f.BackColor = [System.Drawing.Color]::FromArgb(255, 0, 255)".to_string(),
        "$f.TopMost = $true".to_string(),
        "$t = New-Object System.Windows.Forms.TextBox".to_string(),
        "$t.UseSystemPasswordChar = $true".to_string(),
        "$t.AccessibleName = 'Odex test password'".to_string(),
        "$t.Location = New-Object System.Drawing.Point(20, 20)".to_string(),
        "$t.Width = 240".to_string(),
        "$f.Controls.Add($t)".to_string(),
        "$b = New-Object System.Windows.Forms.Button".to_string(),
        "$b.Text = 'Odex post click'".to_string(),
        "$b.Location = New-Object System.Drawing.Point(20, 60)".to_string(),
        "$b.Size = New-Object System.Drawing.Size(240, 40)".to_string(),
        "$b.Add_Click({ $f.Text = $f.Text + ' clicked' })".to_string(),
        "$f.Controls.Add($b)".to_string(),
        "$f.Add_Shown({ $f.Activate(); $t.Focus() })".to_string(),
        "[System.Windows.Forms.Application]::Run($f)".to_string(),
    ]
    .join("; ");
    let child = Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .expect("spawn the PowerShell helper");
    let w = wait_for_window(&WindowMatch::Pid(child.id()), Duration::from_secs(30)).expect("helper window");
    assert_eq!(w.title, title);
    (child, handle(&w))
}

/// (distinct colours, capped at 4096; fraction of magenta pixels)
fn colour_stats(png: &[u8]) -> (usize, f64) {
    let img = image::load_from_memory(png).expect("valid PNG").to_rgb8();
    let mut colours = HashSet::new();
    let mut magenta = 0usize;
    for p in img.pixels() {
        let [r, g, b] = p.0;
        if colours.len() < 4096 {
            colours.insert(p.0);
        }
        if r > 200 && g < 80 && b > 200 {
            magenta += 1;
        }
    }
    (colours.len(), magenta as f64 / (img.width() as f64 * img.height() as f64).max(1.0))
}

/// Closes everything the harness opened, even when an assertion fails.
struct Cleanup {
    notepad: Option<isize>,
    helper: Option<Child>,
}

impl Drop for Cleanup {
    fn drop(&mut self) {
        kill_switch::release();
        if let Some(mut child) = self.helper.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        if let Some(np) = self.notepad.take() {
            if exists(np) {
                // Clear our text first so nothing of ours can be kept by Notepad, then close.
                if let Some(doc) = document(np) {
                    let _ = ui_action(np, &doc.id, UiActionKind::SetValue(String::new()));
                }
                if let Err(e) = close_without_saving(np) {
                    eprintln!("cleanup: {e}");
                }
            }
        }
    }
}

#[test]
#[ignore = "drives real windows; run with --ignored --test-threads=1"]
fn notepad_harness() {
    init_dpi_awareness();
    kill_switch::release();
    security_check().expect("no UAC / credential prompt may be active");
    let mut cleanup = Cleanup { notepad: None, helper: None };

    // 1. Launch our own Notepad window (a new window; other Notepad windows are never touched).
    let np_info = launch_and_wait("notepad.exe", &[], Duration::from_secs(30)).expect("a new Notepad window");
    let np = handle(&np_info);
    cleanup.notepad = Some(np);
    assert!(np_info.app.eq_ignore_ascii_case("notepad.exe"), "{np_info:?}");
    let allowed = vec!["notepad.exe".to_string()];
    let listed = list_windows(&allowed).expect("list windows");
    let ours = listed.iter().find(|w| w.handle == np_info.handle).expect("our window is listed");
    assert!(ours.allowed && is_allowed(ours, &allowed));
    assert!(!is_allowed(ours, &["calc.exe".to_string()]));

    // 2. ui_tree: find the document, set its text through the Value pattern (no focus or cursor needed).
    assert!(wait_for(Duration::from_secs(10), || document(np).is_some()), "Notepad has no Document element");
    let doc = document(np).unwrap();
    assert!(doc.patterns.iter().any(|p| p == "value"), "{doc:?}");
    let tree = ui_tree(np, 40, None, 100_000).unwrap();
    assert!(tree.text.contains(&format!("[{}] Document \"Text editor\"", doc.id)), "{}", tree.text);
    let small = ui_tree(np, 40, None, 300).unwrap();
    assert!(small.truncated && small.text.chars().count() <= 300 && small.text.contains("truncated"));
    let msg = ui_action(np, &doc.id, UiActionKind::SetValue(HELLO.into())).expect("SetValue");
    println!("{msg}");
    assert!(wait_for(Duration::from_secs(5), || document_value(np).as_deref() == Some(HELLO)));
    assert_eq!(document(np).unwrap().id, doc.id, "ids are stable for the same element");

    // 3. Cover Notepad with a topmost helper and capture Notepad in the background.
    let (child, helper) = spawn_password_helper(&np_info.bounds);
    cleanup.helper = Some(child);
    std::thread::sleep(Duration::from_millis(700));
    let shot = screenshot(CaptureTarget::Window(np), 1024).expect("window screenshot");
    assert!(shot.note.is_none(), "expected a PrintWindow capture: {:?}", shot.note);
    assert!(shot.width.max(shot.height) <= 1024);
    let (colours, magenta) = colour_stats(&shot.png);
    println!(
        "window capture {}x{} scale {:.3}: {colours} colours, {magenta:.4} magenta",
        shot.width, shot.height, shot.scale_x
    );
    assert!(colours > 8, "the window capture looks blank");
    assert!(magenta < 0.02, "the window capture shows the covering window ({magenta})");
    // The same area captured from the screen shows the helper on top: Notepad really was occluded.
    let frame = shot.window.as_ref().expect("window info").bounds;
    let region = screenshot(CaptureTarget::Region(frame), 1024).expect("region screenshot");
    let (_, region_magenta) = colour_stats(&region.png);
    println!(
        "screen region capture: {region_magenta:.3} magenta; foreground = {:?}",
        foreground_window().map(|w| w.title)
    );
    assert!(region_magenta > 0.3, "the helper did not cover Notepad ({region_magenta})");
    assert!(!foreground_window().is_some_and(|w| w.handle == np_info.handle), "Notepad should be in the background");

    // Coordinate mapping round-trips through the downscaled screenshot.
    let (cx, cy) = rect_center(&doc.bounds);
    let sx = (cx - shot.origin_x) as f64 / shot.scale_x;
    let sy = (cy - shot.origin_y) as f64 / shot.scale_y;
    let (mx, my) = map_point(&shot, sx, sy, CoordinateSpace::Pixels);
    assert!((mx - cx).abs() <= 2 && (my - cy).abs() <= 2, "({mx},{my}) vs ({cx},{cy})");
    let (nx, ny) = map_point(
        &shot,
        sx / shot.width as f64 * 1000.0,
        sy / shot.height as f64 * 1000.0,
        CoordinateSpace::Normalized1000,
    );
    assert!((nx - cx).abs() <= 2 && (ny - cy).abs() <= 2);

    // 4. Password fields are refused.
    let mut pwd = None;
    assert!(wait_for(Duration::from_secs(10), || {
        pwd = find_elements(helper, "odex test password").ok().and_then(|v| v.into_iter().find(|n| n.is_password));
        pwd.is_some()
    }));
    let pwd = pwd.unwrap();
    assert_eq!(ui_action(helper, &pwd.id, UiActionKind::SetValue("hunter2".into())), Err(Error::PasswordField));
    window_op(helper, WindowOp::Focus).expect("focus helper");
    ui_action(helper, &pwd.id, UiActionKind::Focus).expect("focus password box");
    expect_foreground(helper).expect("helper must be in the foreground");
    let focused = find_elements(helper, "odex test password").unwrap().into_iter().any(|n| n.focused);
    assert!(focused, "the password box must have focus before testing typing refusal");
    assert_eq!(keyboard_type("secret"), Err(Error::PasswordField));
    assert_eq!(keyboard_keys("a"), Err(Error::PasswordField));
    assert_eq!(keyboard_keys("ctrl+v"), Err(Error::PasswordField));
    // Focus moving into the password box part-way through is caught too (Tab from the button wraps to it).
    let button = find_role(helper, "odex post click", "Button").expect("helper button");
    let password_focused =
        || find_elements(helper, "odex test password").unwrap().into_iter().any(|n| n.focused && n.is_password);
    ui_action(helper, &button.id, UiActionKind::Focus).expect("focus button");
    expect_foreground(helper).expect("helper must be in the foreground");
    assert!(!password_focused());
    assert_eq!(keyboard_type("\tsecret"), Err(Error::PasswordField));
    assert!(password_focused());
    ui_action(helper, &button.id, UiActionKind::Focus).expect("focus button");
    expect_foreground(helper).expect("helper must be in the foreground");
    assert_eq!(keyboard_keys("tab a"), Err(Error::PasswordField));
    assert!(password_focused());

    // Background-friendly click: posted mouse messages to a classic (WinForms/Win32) button.
    let (bx, by) = rect_center(&button.bounds);
    post_click(helper, bx, by, MouseButton::Left, false).expect("post_click");
    let clicked = wait_for(Duration::from_secs(3), || {
        window_info(helper, &[]).map(|w| w.title.ends_with(" clicked")).unwrap_or(false)
    });
    assert!(clicked, "post_click should click a classic WinForms button");

    // 5. Kill switch: everything fails until released.
    kill_switch::engage();
    let killed = Err::<(), Error>(Error::KillSwitchEngaged);
    assert_eq!(screenshot(CaptureTarget::Window(np), 512).map(|_| ()), killed);
    assert_eq!(ui_tree(np, 5, None, 1000).map(|_| ()), killed);
    assert_eq!(find_elements(np, "document").map(|_| ()), killed);
    assert_eq!(ui_action(np, &doc.id, UiActionKind::SetValue("must not happen".into())).map(|_| ()), killed);
    assert_eq!(keyboard_type("must not happen"), killed);
    assert_eq!(keyboard_keys("ctrl+a"), killed);
    assert_eq!(mouse(MouseAction::Move, cx, cy), killed);
    assert_eq!(window_op(np, WindowOp::Minimize), killed);
    assert_eq!(launch("notepad.exe", &[]).map(|_| ()), killed);
    assert_eq!(clipboard_get().map(|_| ()), killed);
    assert_eq!(appshot(Some(np), true, 256).map(|_| ()), killed);
    assert!(status(true, &allowed).killed);
    kill_switch::release();
    assert_eq!(document_value(np).as_deref(), Some(HELLO), "nothing changed while killed");

    // 6. Close the helper.
    window_op(helper, WindowOp::Close).expect("close helper");
    assert!(wait_for(Duration::from_secs(10), || !exists(helper)), "helper did not close");
    if let Some(mut child) = cleanup.helper.take() {
        let _ = child.wait();
    }

    // 7. Real input: focus Notepad, click into the text area, type, verify.
    window_op(np, WindowOp::Focus).expect("focus Notepad");
    expect_foreground(np).expect("Notepad must be in the foreground before typing");
    let doc = document(np).unwrap();
    let (cx, cy) = rect_center(&doc.bounds);
    let under = window_at(cx, cy, &allowed).unwrap();
    assert_eq!(under.as_ref().map(|w| w.handle.as_str()), Some(np_info.handle.as_str()));
    assert!(under.unwrap().allowed);
    mouse(MouseAction::Click { button: MouseButton::Left, double: false }, cx, cy).expect("click");
    expect_foreground(np).expect("still in the foreground");
    keyboard_keys("ctrl+end").expect("ctrl+end");
    expect_foreground(np).expect("still in the foreground");
    let typed = " typed: Mixed CASE, punctuation !?@#\"' and \u{e9}\u{fc} \u{20ac} \u{2713}\u{1F600}";
    keyboard_type(typed).expect("type");
    let expected = format!("{HELLO}{typed}");
    assert!(
        wait_for(Duration::from_secs(5), || document_value(np).as_deref() == Some(expected.as_str())),
        "document value: {:?}",
        document_value(np)
    );

    // 8. Appshot: screenshot + compact tree.
    let shot = appshot(Some(np), true, 800).expect("appshot");
    assert!(shot.image_url.starts_with("data:image/png;base64,"));
    assert!(shot.app.eq_ignore_ascii_case("notepad.exe"));
    assert!(shot.width <= 800 && shot.height <= 800);
    let tree_text = shot.ui_tree.unwrap_or_default();
    assert!(tree_text.contains("Document \"Text editor\""), "{tree_text}");
    assert!(tree_text.contains("value=\"hello from odex typed: Mixed CASE, punctuation !?@#"), "{tree_text}");

    // 9. Close without saving: File > Close tab, then "Don't save", all through UI Automation.
    let answered = close_without_saving(np).expect("close Notepad");
    assert!(answered, "expected Notepad to ask about unsaved changes");
    assert!(!exists(np));
    cleanup.notepad = None;
}

#[test]
#[ignore = "needs an interactive desktop"]
fn screen_and_monitor_capture() {
    init_dpi_awareness();
    let all = monitors().expect("monitors");
    assert!(!all.is_empty() && all[0].primary && all[0].index == 0);
    let shot = screenshot(CaptureTarget::Screen, 1568).expect("screen");
    assert!(shot.width.max(shot.height) <= 1568);
    let left = all.iter().map(|m| m.bounds.x as i32).min().unwrap();
    let top = all.iter().map(|m| m.bounds.y as i32).min().unwrap();
    assert_eq!((shot.origin_x, shot.origin_y), (left, top));
    assert!(colour_stats(&shot.png).0 > 8, "screen capture looks blank");
    let m0 = screenshot(CaptureTarget::Monitor(0), 0).expect("primary monitor");
    assert_eq!((m0.width, m0.height), (all[0].bounds.width as u32, all[0].bounds.height as u32));
    assert_eq!((m0.scale_x, m0.scale_y), (1.0, 1.0));
    assert!(matches!(screenshot(CaptureTarget::Monitor(99), 100), Err(Error::NotFound(_))));
    assert!(screenshot(CaptureTarget::Window(0), 100).is_err());
}

#[test]
#[ignore = "touches the real clipboard (restores its text afterwards)"]
fn clipboard_round_trip() {
    let original = clipboard_get().expect("read clipboard");
    if original.is_empty() {
        // The clipboard may hold non-text data we couldn't restore; don't clobber it.
        eprintln!("skipping: clipboard has no text");
        return;
    }
    let probe = format!("odex clipboard probe {} \u{2713} \u{fc}n\u{ef}c\u{f6}d\u{e9} \u{1F600}", std::process::id());
    clipboard_set(&probe).expect("set");
    let got = clipboard_get().expect("get");
    clipboard_set(&original).expect("restore");
    assert_eq!(got, probe);
    assert_eq!(clipboard_get().unwrap(), original);
}
