//! GDI layered renderer: the frame is drawn into a color DIB (runs on black) and
//! a coverage-mask DIB (the same runs in white), composed to premultiplied BGRA
//! and pushed with `UpdateLayeredWindow`.

use std::mem::size_of;
use std::sync::atomic::{AtomicBool, Ordering};

use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM};
use windows::Win32::Graphics::Gdi::{
    AC_SRC_ALPHA, AC_SRC_OVER, ANTIALIASED_QUALITY, BI_RGB, BITMAPINFO, BITMAPINFOHEADER,
    BLENDFUNCTION, CLIP_DEFAULT_PRECIS, CreateCompatibleDC, CreateDIBSection, DEFAULT_CHARSET,
    DIB_RGB_COLORS, DeleteDC, DeleteObject, ETO_OPTIONS, ExtTextOutW, FW_NORMAL, FW_SEMIBOLD,
    GetMonitorInfoW, GetTextExtentPoint32W, GetTextMetricsW, HBITMAP, HDC, HFONT, HGDIOBJ,
    MONITOR_DEFAULTTONEAREST, MONITOR_DEFAULTTOPRIMARY, MONITORINFO, OUT_DEFAULT_PRECIS,
    SelectObject, SetBkMode, SetTextColor, TEXTMETRICW, TRANSPARENT,
};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, RegisterClassW, SW_HIDE, SW_SHOWNOACTIVATE,
    ShowWindow, ULW_ALPHA, UpdateLayeredWindow, WINDOW_EX_STYLE, WNDCLASSW, WS_EX_LAYERED,
    WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
};
use windows::core::{Error, PCWSTR, Result, w};

use crate::config::{Config, Corner};
use crate::layout::{Frame, Rgb};

const CLASS: PCWSTR = w!("FpsOverlay");

const WHITE_RGB: Rgb = Rgb(255, 255, 255);
const SHADOW_RGB: Rgb = Rgb(0, 0, 0);

/// `cov` from the mask's red channel, `a = cov + bg·(1−cov)`; rgb is already
/// premultiplied because it was drawn on black. `out` is BGRA as `0xAARRGGBB`.
pub fn compose(color: &[u32], mask: &[u32], bg_alpha: u8, out: &mut [u32]) {
    let bg = bg_alpha as f32 / 255.0;
    for ((c, m), o) in color.iter().zip(mask).zip(out) {
        let cov = ((m >> 16) & 0xff) as f32 / 255.0;
        let a = cov + bg * (1.0 - cov);
        *o = ((a * 255.0).round() as u32) << 24 | (c & 0x00ff_ffff);
    }
}

/// A top-down 32-bit DIB section with its own memory DC.
struct Dib {
    mem: HDC,
    bmp: HBITMAP,
    old: HGDIOBJ,
    bits: *mut u32,
    w: i32,
    h: i32,
}

impl Dib {
    fn new(w: i32, h: i32) -> Result<Self> {
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: -h, // top-down: row 0 is the top
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        unsafe {
            let mem = CreateCompatibleDC(None);
            let mut bits = std::ptr::null_mut();
            let bmp = match CreateDIBSection(Some(mem), &info, DIB_RGB_COLORS, &mut bits, None, 0) {
                Ok(bmp) => bmp,
                Err(e) => {
                    let _ = DeleteDC(mem);
                    return Err(e);
                }
            };
            let old = SelectObject(mem, HGDIOBJ(bmp.0));
            Ok(Self {
                mem,
                bmp,
                old,
                bits: bits.cast(),
                w,
                h,
            })
        }
    }

    fn px(&self) -> &[u32] {
        // SAFETY: bits points at w*h u32s owned by this DIB.
        unsafe { std::slice::from_raw_parts(self.bits, (self.w * self.h) as usize) }
    }

    fn fill(&self, v: u32) {
        // SAFETY: as `px`; the DIB is not readable via a live slice here.
        unsafe { std::slice::from_raw_parts_mut(self.bits, (self.w * self.h) as usize) }.fill(v);
    }

    fn write(&self, src: &[u32]) {
        // SAFETY: as `px`; `src` is never this DIB's memory.
        unsafe { std::slice::from_raw_parts_mut(self.bits, (self.w * self.h) as usize) }
            .copy_from_slice(src);
    }
}

impl Drop for Dib {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.mem, self.old);
            let _ = DeleteObject(HGDIOBJ(self.bmp.0));
            let _ = DeleteDC(self.mem);
        }
    }
}

pub struct Overlay {
    hwnd: HWND,
    dc: HDC, // measuring DC: no bitmap, only fonts are selected into it
    color: Option<Dib>,
    mask: Option<Dib>,
    out: Vec<u32>,
    fonts: Vec<(i32, u32, HFONT)>,
    cfg: Config,
}

impl Overlay {
    pub fn new(cfg: &Config) -> Result<Self> {
        static REGISTERED: AtomicBool = AtomicBool::new(false);
        unsafe {
            if !REGISTERED.swap(true, Ordering::Relaxed) {
                let wc = WNDCLASSW {
                    lpfnWndProc: Some(wndproc),
                    lpszClassName: CLASS,
                    ..Default::default()
                };
                if RegisterClassW(&wc) == 0 {
                    REGISTERED.store(false, Ordering::Relaxed);
                    return Err(Error::from_thread());
                }
            }
            let ex_style: WINDOW_EX_STYLE = WS_EX_TOPMOST
                | WS_EX_LAYERED
                | WS_EX_TRANSPARENT
                | WS_EX_NOACTIVATE
                | WS_EX_TOOLWINDOW;
            let hwnd = CreateWindowExW(
                ex_style,
                CLASS,
                w!(""),
                WS_POPUP,
                0,
                0,
                0,
                0,
                None,
                None,
                None,
                None,
            )?;
            Ok(Self {
                hwnd,
                dc: CreateCompatibleDC(None),
                color: None,
                mask: None,
                out: Vec::new(),
                fonts: Vec::new(),
                cfg: cfg.clone(),
            })
        }
    }

    pub fn draw(&mut self, frame: &Frame, anchor: HWND) -> Result<()> {
        let dpi = dpi(anchor);
        let (w, h) = self.bake(frame, dpi)?;
        let (x, y) = self.place(w, h, anchor);
        unsafe {
            let dst = POINT { x, y };
            let size = SIZE { cx: w, cy: h };
            let src = POINT::default();
            let blend = BLENDFUNCTION {
                BlendOp: AC_SRC_OVER as u8,
                BlendFlags: 0,
                SourceConstantAlpha: 255,
                AlphaFormat: AC_SRC_ALPHA as u8,
            };
            let color = self.color.as_ref().expect("bake created the DIBs");
            UpdateLayeredWindow(
                self.hwnd,
                None,
                Some(&dst),
                Some(&size),
                Some(color.mem),
                Some(&src),
                COLORREF(0),
                Some(&blend),
                ULW_ALPHA,
            )?;
            let _ = ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
        }
        Ok(())
    }

    pub fn hide(&self) {
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_HIDE);
        }
    }

    pub fn release_buffers(&mut self) {
        self.color = None;
        self.mask = None;
        self.out = Vec::new();
        for (_, _, font) in self.fonts.drain(..) {
            unsafe {
                let _ = DeleteObject(HGDIOBJ(font.0));
            }
        }
    }

    /// Draws the frame into the DIBs and composes into `self.out`; returns its size.
    fn bake(&mut self, frame: &Frame, dpi: f32) -> Result<(i32, i32)> {
        let s = self.cfg.scale * dpi / 96.0;
        let pad = (frame.padding.0 * s).round() as i32;
        let pady = (frame.padding.1 * s).round() as i32;

        // Per line: font, height, gap+run widths.
        let mut lines = Vec::with_capacity(frame.lines.len());
        for line in &frame.lines {
            let px = ((frame.font_px * line.size * s).round() as i32).max(1);
            let font = self.font(px, frame.semibold);
            let mut tm = TEXTMETRICW::default();
            let mut widths = Vec::with_capacity(line.runs.len());
            let mut width = 0;
            unsafe {
                SelectObject(self.dc, HGDIOBJ(font.0));
                let _ = GetTextMetricsW(self.dc, &mut tm);
            }
            for run in &line.runs {
                let gap = (run.gap_before * s).round() as i32;
                let w = text_width(self.dc, &run.text);
                width += gap + w;
                widths.push((gap, w));
            }
            lines.push((font, tm.tmHeight.max(1), width, widths));
        }

        let w = (lines.iter().map(|l| l.2).max().unwrap_or(0) + 2 * pad).max(1);
        let h = (lines.iter().map(|l| l.1).sum::<i32>() + 2 * pady).max(1);
        if stale(&self.color, w, h) || stale(&self.mask, w, h) {
            self.color = Some(Dib::new(w, h)?);
            self.mask = Some(Dib::new(w, h)?);
        }

        let bg = if frame.background {
            (self.cfg.background_opacity * 255.0).round() as u8
        } else {
            0
        };
        let color = self.color.as_ref().expect("DIBs exist");
        let mask = self.mask.as_ref().expect("DIBs exist");
        color.fill(0);
        mask.fill(u32::from(bg) * 0x0001_0101);

        let mut y = pady;
        for (line, (font, height, _, widths)) in frame.lines.iter().zip(&lines) {
            let mut x = pad;
            unsafe {
                SelectObject(color.mem, HGDIOBJ(font.0));
                SelectObject(mask.mem, HGDIOBJ(font.0));
                SetBkMode(color.mem, TRANSPARENT);
                SetBkMode(mask.mem, TRANSPARENT);
            }
            for (run, (gap, run_width)) in line.runs.iter().zip(widths) {
                x += gap;
                if frame.shadow {
                    text_out(color.mem, x + 1, y + 1, &run.text, SHADOW_RGB);
                    text_out(mask.mem, x + 1, y + 1, &run.text, WHITE_RGB);
                }
                text_out(color.mem, x, y, &run.text, run.color);
                text_out(mask.mem, x, y, &run.text, WHITE_RGB);
                x += run_width;
            }
            y += height;
        }

        self.out.resize((w * h) as usize, 0);
        compose(color.px(), mask.px(), bg, &mut self.out);
        color.write(&self.out);
        Ok((w, h))
    }

    fn font(&mut self, px: i32, semibold: bool) -> HFONT {
        let weight = if semibold { FW_SEMIBOLD.0 } else { FW_NORMAL.0 };
        if let Some(&(_, _, font)) = self.fonts.iter().find(|(p, w, _)| *p == px && *w == weight) {
            return font;
        }
        let font = unsafe {
            windows::Win32::Graphics::Gdi::CreateFontW(
                -px,
                0,
                0,
                0,
                weight as i32,
                0,
                0,
                0,
                DEFAULT_CHARSET,
                OUT_DEFAULT_PRECIS,
                CLIP_DEFAULT_PRECIS,
                ANTIALIASED_QUALITY,
                0, // DEFAULT_PITCH | FF_DONTCARE
                w!("Segoe UI"),
            )
        };
        self.fonts.push((px, weight, font));
        font
    }

    fn place(&self, w: i32, h: i32, anchor: HWND) -> (i32, i32) {
        let mut mi = MONITORINFO {
            cbSize: size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        let mon = unsafe {
            windows::Win32::Graphics::Gdi::MonitorFromWindow(anchor, monitor_flag(anchor))
        };
        let ok = unsafe { GetMonitorInfoW(mon, &mut mi).as_bool() };
        let r: RECT = if ok { mi.rcMonitor } else { RECT::default() };
        match self.cfg.position {
            Corner::TopLeft => (r.left, r.top),
            Corner::TopRight => (r.right - w, r.top),
            Corner::BottomLeft => (r.left, r.bottom - h),
            Corner::BottomRight => (r.right - w, r.bottom - h),
        }
    }
}

impl Drop for Overlay {
    fn drop(&mut self) {
        self.release_buffers();
        unsafe {
            let _ = DeleteDC(self.dc);
            let _ = DestroyWindow(self.hwnd);
        }
    }
}

/// A DIB that is absent or not the wanted size must be (re)created.
fn stale(dib: &Option<Dib>, w: i32, h: i32) -> bool {
    dib.as_ref().is_none_or(|d| d.w != w || d.h != h)
}

fn monitor_flag(anchor: HWND) -> windows::Win32::Graphics::Gdi::MONITOR_FROM_FLAGS {
    if anchor.0.is_null() {
        MONITOR_DEFAULTTOPRIMARY
    } else {
        MONITOR_DEFAULTTONEAREST
    }
}

/// Effective DPI of the anchor window's monitor, 96 when unknown.
fn dpi(anchor: HWND) -> f32 {
    let mon =
        unsafe { windows::Win32::Graphics::Gdi::MonitorFromWindow(anchor, monitor_flag(anchor)) };
    let (mut x, mut y) = (96u32, 96u32);
    unsafe {
        let _ = GetDpiForMonitor(mon, MDT_EFFECTIVE_DPI, &mut x, &mut y);
    }
    if x == 0 { 96.0 } else { x as f32 }
}

fn text_width(hdc: HDC, text: &str) -> i32 {
    let wide: Vec<u16> = text.encode_utf16().collect();
    let mut size = SIZE::default();
    unsafe {
        let _ = GetTextExtentPoint32W(hdc, &wide, &mut size);
    }
    size.cx
}

fn text_out(hdc: HDC, x: i32, y: i32, text: &str, rgb: Rgb) {
    let wide: Vec<u16> = text.encode_utf16().collect();
    let colorref = COLORREF((rgb.0 as u32) | (rgb.1 as u32) << 8 | (rgb.2 as u32) << 16);
    unsafe {
        SetTextColor(hdc, colorref);
        let _ = ExtTextOutW(
            hdc,
            x,
            y,
            ETO_OPTIONS(0),
            None,
            PCWSTR(wide.as_ptr()),
            wide.len() as u32,
            None,
        );
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WHITE: u32 = 0x00FF_FFFF;

    #[test]
    fn transparent_bg_no_text() {
        let (color, mask) = (vec![0u32; 4], vec![0u32; 4]);
        let mut out = vec![0u32; 4];
        compose(&color, &mask, 0, &mut out);
        assert_eq!(out, vec![0u32; 4]);
    }

    #[test]
    fn opaque_bg_no_text() {
        let (color, mask) = (vec![0u32; 4], vec![0u32; 4]);
        let mut out = vec![0u32; 4];
        compose(&color, &mask, 255, &mut out);
        assert_eq!(out, vec![0xFF00_0000u32; 4]);
    }

    #[test]
    fn full_text_pixel() {
        let color = vec![0x00C8_6432u32]; // R 200, G 100, B 50
        let mask = vec![WHITE];
        let mut out = vec![0u32];
        compose(&color, &mask, 0, &mut out);
        assert_eq!(out[0], 0xFFC8_6432);
    }

    #[test]
    fn half_cov_half_bg() {
        let color = vec![0u32];
        let mask = vec![0x0080_8080u32]; // cov ≈ 0.502
        let mut out = vec![0u32];
        compose(&color, &mask, 128, &mut out);
        let alpha = out[0] >> 24;
        assert!((190..=194).contains(&alpha), "alpha {alpha}");
    }

    #[test]
    fn shadow_pixel() {
        let color = vec![0u32]; // the shadow is drawn in black
        let mask = vec![WHITE];
        let mut out = vec![0u32];
        compose(&color, &mask, 0, &mut out);
        assert_eq!(out[0], 0xFF00_0000);
    }
}
