//! Screen and window capture via GDI (`BitBlt`, `PrintWindow(PW_RENDERFULLCONTENT)`), PNG encoding.

use image::codecs::png::{CompressionType, FilterType as PngFilter, PngEncoder};
use image::imageops::FilterType;
use image::{ExtendedColorType, ImageEncoder, RgbImage};
use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::Graphics::Gdi::{
    BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject, GetDC, GetDIBits, ReleaseDC,
    SelectObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, CAPTUREBLT, DIB_RGB_COLORS, HBITMAP, HDC, SRCCOPY,
};
use windows::Win32::Storage::Xps::{PrintWindow, PRINT_WINDOW_FLAGS};
use windows::Win32::UI::WindowsAndMessaging::IsIconic;

use super::window::{frame_rect, info_for, init_dpi_awareness, monitors, require_window, virtual_screen, window_rect};
use super::{last_error, win_err};
use crate::{fit_within, CaptureTarget, Error, Result, Screenshot, WindowInfo};

/// `PW_RENDERFULLCONTENT`: capture DirectComposition / GPU-rendered content too (Windows 8.1+).
const PW_RENDERFULLCONTENT: u32 = 0x2;
/// Refuse absurd capture sizes (~ 16K x 8K).
const MAX_PIXELS: u64 = 16_384 * 8_192;

/// Raw top-down BGRA pixels.
struct Raw {
    width: u32,
    height: u32,
    bgra: Vec<u8>,
}

pub fn screenshot(target: CaptureTarget, max_edge_px: u32) -> Result<Screenshot> {
    init_dpi_awareness();
    let (raw, (ox, oy), window, note) = match target {
        CaptureTarget::Screen => {
            let (x, y, w, h) = virtual_screen();
            (grab_screen(x, y, w, h)?, (x, y), None, None)
        }
        CaptureTarget::Monitor(i) => {
            let all = monitors()?;
            let m =
                all.get(i).ok_or_else(|| Error::NotFound(format!("monitor {i} (there are {} monitors)", all.len())))?;
            let b = m.bounds;
            let (x, y) = (b.x as i32, b.y as i32);
            (grab_screen(x, y, b.width as i32, b.height as i32)?, (x, y), None, None)
        }
        CaptureTarget::Region(r) => {
            let (x, y) = (r.x.round() as i32, r.y.round() as i32);
            let (w, h) = (r.width.round() as i32, r.height.round() as i32);
            (grab_screen(x, y, w, h)?, (x, y), None, None)
        }
        CaptureTarget::Window(hwnd) => capture_window(hwnd)?,
    };
    encode(raw, ox, oy, max_edge_px, window, note)
}

type WindowCapture = (Raw, (i32, i32), Option<WindowInfo>, Option<String>);

fn capture_window(hwnd: isize) -> Result<WindowCapture> {
    let h = require_window(hwnd)?;
    if unsafe { IsIconic(h) }.as_bool() {
        return Err(Error::Other("the window is minimized; restore it (window restore) before capturing it".into()));
    }
    let wr = window_rect(h);
    let fr = frame_rect(h);
    let (ww, wh) = (wr.right - wr.left, wr.bottom - wr.top);
    if ww <= 0 || wh <= 0 {
        return Err(Error::Other("the window has no visible area".into()));
    }
    let info = info_for(h, &[]);
    // PrintWindow renders the window itself, so occluded and background windows work.
    if let Ok(full) = print_window(h, ww, wh) {
        if !is_blank(&full) {
            // Crop the invisible resize borders / shadow to the visible frame.
            let crop = RECT {
                left: (fr.left - wr.left).clamp(0, ww),
                top: (fr.top - wr.top).clamp(0, wh),
                right: (fr.right - wr.left).clamp(0, ww),
                bottom: (fr.bottom - wr.top).clamp(0, wh),
            };
            let (raw, ox, oy) = if crop.right > crop.left && crop.bottom > crop.top {
                (crop_raw(&full, crop), wr.left + crop.left, wr.top + crop.top)
            } else {
                (full, wr.left, wr.top)
            };
            return Ok((raw, (ox, oy), Some(info), None));
        }
    }
    let raw = grab_screen(fr.left, fr.top, fr.right - fr.left, fr.bottom - fr.top)?;
    Ok((
        raw,
        (fr.left, fr.top),
        Some(info),
        Some(
            "PrintWindow returned no content, so the screen area was captured instead; windows on top of it may \
             be visible"
                .into(),
        ),
    ))
}

fn check_size(w: i32, h: i32) -> Result<()> {
    if w <= 0 || h <= 0 {
        return Err(Error::Other(format!("invalid capture size {w}x{h}")));
    }
    if (w as u64) * (h as u64) > MAX_PIXELS {
        return Err(Error::Other(format!("capture too large ({w}x{h})")));
    }
    Ok(())
}

/// Memory DC + bitmap; cleaned up on drop.
struct Canvas {
    screen: HDC,
    mem: HDC,
    bmp: HBITMAP,
    width: i32,
    height: i32,
}

impl Canvas {
    fn new(width: i32, height: i32) -> Result<Self> {
        check_size(width, height)?;
        unsafe {
            let screen = GetDC(HWND::default());
            if screen.is_invalid() {
                return Err(last_error("GetDC"));
            }
            let mem = CreateCompatibleDC(screen);
            let bmp = CreateCompatibleBitmap(screen, width, height);
            if mem.is_invalid() || bmp.is_invalid() {
                if !bmp.is_invalid() {
                    let _ = DeleteObject(bmp);
                }
                if !mem.is_invalid() {
                    let _ = DeleteDC(mem);
                }
                ReleaseDC(HWND::default(), screen);
                return Err(Error::Win("could not allocate a capture bitmap".into()));
            }
            Ok(Canvas { screen, mem, bmp, width, height })
        }
    }

    /// Run `draw` with the bitmap selected into the memory DC, then read the pixels.
    fn draw(&self, draw: impl FnOnce(HDC) -> Result<()>) -> Result<Raw> {
        unsafe {
            let old = SelectObject(self.mem, self.bmp);
            let res = draw(self.mem);
            SelectObject(self.mem, old);
            res?;
            // GetDIBits requires the bitmap not to be selected into a DC.
            let mut bmi = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: self.width,
                    biHeight: -self.height, // top-down
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut bgra = vec![0u8; self.width as usize * self.height as usize * 4];
            let lines = GetDIBits(
                self.screen,
                self.bmp,
                0,
                self.height as u32,
                Some(bgra.as_mut_ptr() as *mut _),
                &mut bmi,
                DIB_RGB_COLORS,
            );
            if lines == 0 {
                return Err(last_error("GetDIBits"));
            }
            Ok(Raw { width: self.width as u32, height: self.height as u32, bgra })
        }
    }
}

impl Drop for Canvas {
    fn drop(&mut self) {
        unsafe {
            let _ = DeleteObject(self.bmp);
            let _ = DeleteDC(self.mem);
            ReleaseDC(HWND::default(), self.screen);
        }
    }
}

fn grab_screen(x: i32, y: i32, w: i32, h: i32) -> Result<Raw> {
    let canvas = Canvas::new(w, h)?;
    let screen = canvas.screen;
    canvas.draw(|mem| unsafe {
        BitBlt(mem, 0, 0, w, h, screen, x, y, SRCCOPY | CAPTUREBLT).map_err(|e| win_err("BitBlt", e))
    })
}

fn print_window(hwnd: HWND, w: i32, h: i32) -> Result<Raw> {
    let canvas = Canvas::new(w, h)?;
    canvas.draw(|mem| unsafe {
        if PrintWindow(hwnd, mem, PRINT_WINDOW_FLAGS(PW_RENDERFULLCONTENT)).as_bool() {
            Ok(())
        } else {
            Err(last_error("PrintWindow"))
        }
    })
}

/// A capture where every pixel has the same color (PrintWindow failure mode for some GPU apps).
fn is_blank(raw: &Raw) -> bool {
    let mut px = raw.bgra.chunks_exact(4);
    let Some(first) = px.next() else { return true };
    px.all(|p| p[..3] == first[..3])
}

fn crop_raw(raw: &Raw, r: RECT) -> Raw {
    let (w, h) = ((r.right - r.left) as usize, (r.bottom - r.top) as usize);
    let stride = raw.width as usize * 4;
    let mut bgra = Vec::with_capacity(w * h * 4);
    for row in r.top as usize..r.bottom as usize {
        let start = row * stride + r.left as usize * 4;
        bgra.extend_from_slice(&raw.bgra[start..start + w * 4]);
    }
    Raw { width: w as u32, height: h as u32, bgra }
}

fn encode(
    raw: Raw,
    origin_x: i32,
    origin_y: i32,
    max_edge_px: u32,
    window: Option<WindowInfo>,
    note: Option<String>,
) -> Result<Screenshot> {
    let (w, h) = (raw.width, raw.height);
    let mut rgb = Vec::with_capacity(w as usize * h as usize * 3);
    for p in raw.bgra.chunks_exact(4) {
        rgb.extend_from_slice(&[p[2], p[1], p[0]]);
    }
    let img = RgbImage::from_raw(w, h, rgb).ok_or_else(|| Error::Other("bad capture buffer".into()))?;
    let (nw, nh) = fit_within(w, h, max_edge_px);
    let img = if (nw, nh) != (w, h) { image::imageops::resize(&img, nw, nh, FilterType::Triangle) } else { img };
    let mut png = Vec::new();
    PngEncoder::new_with_quality(&mut png, CompressionType::Fast, PngFilter::Adaptive)
        .write_image(img.as_raw(), nw, nh, ExtendedColorType::Rgb8)
        .map_err(|e| Error::Other(format!("PNG encoding failed: {e}")))?;
    Ok(Screenshot {
        png,
        width: nw,
        height: nh,
        scale_x: w as f64 / nw as f64,
        scale_y: h as f64 / nh as f64,
        origin_x,
        origin_y,
        window,
        note,
    })
}
