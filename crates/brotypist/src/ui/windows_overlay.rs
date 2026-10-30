//! Same-line GDI presentation. No ordinary-completion popup fallback.
use super::wide;
use crate::{
    core::{Authority, InlineRender, Rect, SnapshotKey},
    runtime::Command,
};
use anyhow::{Result, ensure};
use serde::Deserialize;
use serde_json::Value;
use windows::{
    Win32::{
        Foundation::*,
        Graphics::Gdi::*,
        UI::{HiDpi::GetDpiForWindow, WindowsAndMessaging::*},
    },
    core::{PCWSTR, w},
};

#[derive(Clone, Deserialize)]
pub struct Presentation {
    pub key: SnapshotKey,
    pub authority: Authority,
    pub revision: u64,
    pub text: String,
    pub mode: String,
    pub rect: Option<Rect>,
    pub render: Option<InlineRender>,
    #[serde(default)]
    pub choices: Vec<String>,
}
impl Presentation {
    pub fn ack(&self, text: String) -> Command {
        Command::PresentationResult {
            key: self.key.clone(),
            authority: self.authority.clone(),
            revision: self.revision,
            visible_text: text,
            mode: self.mode.clone(),
        }
    }
}
struct Paint {
    text: Vec<u16>,
    font: HFONT,
    y: i32,
    color: COLORREF,
    transparent: bool,
    width: i32,
    height: i32,
    painted: std::cell::Cell<bool>,
}
pub struct Overlay {
    window: HWND,
    paint: Box<Paint>,
    pub current: Option<Presentation>,
    foreground: HWND,
    dpi: u32,
    pub visible: String,
}
unsafe extern "system" fn procedure(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if msg == WM_NCHITTEST {
        return LRESULT(HTTRANSPARENT as isize);
    }
    if msg == WM_MOUSEACTIVATE {
        return LRESULT(MA_NOACTIVATE as isize);
    }
    if msg == WM_PAINT {
        unsafe {
            let data = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const Paint;
            let mut ps = PAINTSTRUCT::default();
            let dc = BeginPaint(hwnd, &mut ps);
            if let Some(p) = data.as_ref() {
                let brush = CreateSolidBrush(if p.transparent {
                    COLORREF(0xff00ff)
                } else {
                    COLORREF(GetSysColor(COLOR_WINDOW))
                });
                FillRect(
                    dc,
                    &RECT {
                        left: 0,
                        top: 0,
                        right: p.width,
                        bottom: p.height,
                    },
                    brush,
                );
                let _ = DeleteObject(brush.into());
                let old = SelectObject(dc, p.font.into());
                SetBkMode(dc, TRANSPARENT);
                SetTextColor(dc, p.color);
                p.painted.set(TextOutW(dc, 0, p.y, &p.text).as_bool());
                let _ = SelectObject(dc, old);
            }
            let _ = EndPaint(hwnd, &ps);
            return LRESULT(0);
        }
    }
    unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
}
impl Overlay {
    pub fn new(instance: HINSTANCE) -> Result<Self> {
        unsafe {
            RegisterClassW(&WNDCLASSW {
                lpfnWndProc: Some(procedure),
                hInstance: instance,
                lpszClassName: w!("BroTypist.GDI.Inline"),
                ..Default::default()
            });
            let window = CreateWindowExW(
                WS_EX_NOACTIVATE
                    | WS_EX_TOOLWINDOW
                    | WS_EX_TOPMOST
                    | WS_EX_TRANSPARENT
                    | WS_EX_LAYERED,
                w!("BroTypist.GDI.Inline"),
                w!("BroTypist suggestion"),
                WS_POPUP,
                0,
                0,
                1,
                1,
                None,
                None,
                Some(instance),
                None,
            )?;
            SetLayeredWindowAttributes(window, COLORREF(0xff00ff), 255, LWA_COLORKEY)?;
            let mut paint = Box::new(Paint {
                text: Vec::new(),
                font: HFONT::default(),
                y: 0,
                color: COLORREF(0x888888),
                transparent: true,
                width: 1,
                height: 1,
                painted: std::cell::Cell::new(false),
            });
            SetWindowLongPtrW(window, GWLP_USERDATA, (&mut *paint as *mut Paint) as isize);
            Ok(Self {
                window,
                paint,
                current: None,
                foreground: HWND::default(),
                dpi: 0,
                visible: String::new(),
            })
        }
    }
    pub fn hide(&mut self) -> Option<Command> {
        unsafe {
            let _ = ShowWindow(self.window, SW_HIDE);
        }
        self.visible.clear();
        self.current.take().map(|p| p.ack(String::new()))
    }
    pub fn valid(&self) -> bool {
        unsafe {
            self.current.is_none()
                || (GetForegroundWindow() == self.foreground
                    && IsWindowVisible(self.window).as_bool()
                    && !IsIconic(self.foreground).as_bool()
                    && GetDpiForWindow(self.foreground) == self.dpi
                    && self.paint.painted.get())
        }
    }
    pub fn show(&mut self, event: &Value) -> Result<Command> {
        let p: Presentation = serde_json::from_value(event.clone())?;
        let _ = self.hide();
        self.current = Some(p.clone());
        let result = self.present(&p);
        match result {
            Ok(text) => {
                self.visible = text.clone();
                Ok(p.ack(text))
            }
            Err(_) => {
                unsafe {
                    let _ = ShowWindow(self.window, SW_HIDE);
                }
                Ok(p.ack(String::new()))
            }
        }
    }
    fn present(&mut self, p: &Presentation) -> Result<String> {
        unsafe {
            ensure!(
                p.mode == "completion" || p.mode == "correction",
                "Chooser uses a separate explicit surface."
            );
            let caret = p.rect.ok_or_else(|| anyhow::anyhow!("No caret"))?;
            let r = p
                .render
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("No verified font"))?;
            ensure!(
                r.valid(Some(caret)) && r.source == "windows-uia-font-run" && r.suffix.is_empty(),
                "Unverified or occupied row"
            );
            self.foreground = GetForegroundWindow();
            ensure!(!self.foreground.0.is_null(), "No foreground window");
            let dpi = GetDpiForWindow(self.foreground);
            ensure!(dpi > 0, "Unknown DPI");
            self.dpi = dpi;
            let name = wide(&r.font_name);
            let font = CreateFontW(
                -(r.font_size * dpi as f64 / 72.0).round_ties_even() as i32,
                0,
                0,
                0,
                r.font_weight,
                u32::from(r.italic),
                0,
                0,
                DEFAULT_CHARSET,
                OUT_DEFAULT_PRECIS,
                CLIP_DEFAULT_PRECIS,
                DEFAULT_QUALITY,
                0,
                PCWSTR(name.as_ptr()),
            );
            ensure!(!font.0.is_null(), "Font unavailable");
            let dc = CreateCompatibleDC(None);
            if dc.0.is_null() {
                let _ = DeleteObject(font.into());
                anyhow::bail!("No layout DC");
            }
            let old = SelectObject(dc, font.into());
            let mut tm = TEXTMETRICW::default();
            let mut face = [0u16; 256];
            let measured =
                GetTextMetricsW(dc, &mut tm).as_bool() && GetTextFaceW(dc, Some(&mut face)) > 0;
            let face = String::from_utf16_lossy(
                &face[..face.iter().position(|c| *c == 0).unwrap_or(face.len())],
            );
            let available = (r.clip_rect[0] + r.clip_rect[2] - caret[0]).floor() as i32;
            let fitted = if measured
                && (tm.tmAscent as f64 - r.ascent).abs() <= 1.0
                && (tm.tmDescent as f64 - r.descent).abs() <= 1.0
                && crate::core::casefold(&face) == crate::core::casefold(&r.font_name)
            {
                fit_text(dc, &p.text, available)
            } else {
                None
            };
            let _ = SelectObject(dc, old);
            let _ = DeleteDC(dc);
            let Some((text, units, size)) =
                fitted.filter(|_| GetForegroundWindow() == self.foreground)
            else {
                let _ = DeleteObject(font.into());
                anyhow::bail!("No visible same-line prefix");
            };
            if !self.paint.font.0.is_null() {
                let _ = DeleteObject(self.paint.font.into());
            }
            self.paint.font = font;
            self.paint.text = units;
            self.paint.y = 0;
            self.paint.transparent = true;
            self.paint.color = COLORREF(0x888888);
            self.paint.width = size.cx.max(1);
            self.paint.height = tm.tmHeight;
            self.paint.painted.set(false);
            SetWindowPos(
                self.window,
                Some(HWND_TOPMOST),
                caret[0].ceil() as i32,
                (r.baseline - r.ascent).round() as i32,
                size.cx,
                tm.tmHeight,
                SWP_NOACTIVATE | SWP_SHOWWINDOW,
            )?;
            let _ = InvalidateRect(Some(self.window), None, true);
            let _ = UpdateWindow(self.window);
            ensure!(
                self.paint.painted.get()
                    && IsWindowVisible(self.window).as_bool()
                    && GetForegroundWindow() == self.foreground,
                "Presentation refused"
            );
            Ok(text)
        }
    }
}
impl Drop for Overlay {
    fn drop(&mut self) {
        unsafe {
            let _ = ShowWindow(self.window, SW_HIDE);
            SetWindowLongPtrW(self.window, GWLP_USERDATA, 0);
            let _ = DestroyWindow(self.window);
            if !self.paint.font.0.is_null() {
                let _ = DeleteObject(self.paint.font.into());
            }
        }
    }
}
/// Native GDI returns a UTF-16 fitting boundary; never acknowledge half a scalar.
pub fn fitted_prefix(text: &str, fit: usize) -> (&str, usize) {
    if text
        .chars()
        .any(|c| c.is_control() || matches!(c, '\u{2028}' | '\u{2029}'))
    {
        return ("", 0);
    }
    let (mut end, mut units) = (0, 0);
    for (index, c) in text.char_indices() {
        let next = units + c.len_utf16();
        if next > fit {
            break;
        }
        units = next;
        end = index + c.len_utf8();
    }
    (&text[..end], units)
}
pub unsafe fn fit_text(dc: HDC, text: &str, width: i32) -> Option<(String, Vec<u16>, SIZE)> {
    unsafe {
        if width <= 0 {
            return None;
        }
        let mut units: Vec<_> = text.encode_utf16().collect();
        let mut fit = 0i32;
        let mut size = SIZE::default();
        let length = i32::try_from(units.len()).ok()?;
        if !GetTextExtentExPointW(
            dc,
            PCWSTR(units.as_ptr()),
            length,
            width,
            Some(&mut fit),
            None,
            &mut size,
        )
        .as_bool()
            || fit < 0
            || fit > length
        {
            return None;
        }
        let (prefix, count) = fitted_prefix(text, fit as usize);
        if prefix.is_empty() {
            return None;
        }
        units.truncate(count);
        if !GetTextExtentPoint32W(dc, &units, &mut size).as_bool()
            || size.cx > width
            || size.cx <= 0
        {
            return None;
        }
        Some((prefix.into(), units, size))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_complete_native_utf16_prefix_is_acknowledged() {
        assert_eq!(fitted_prefix("a😀bc", 2), ("a", 1));
        assert_eq!(fitted_prefix("a😀bc", 3), ("a😀", 3));
        assert_eq!(fitted_prefix("private\nnext", 100), ("", 0));
        assert_eq!(fitted_prefix("abc", 0), ("", 0));
    }
    #[test]
    fn refusal_ack_carries_exact_authority() {
        let p = Presentation {
            key: crate::core::Snapshot::default().key(),
            authority: crate::core::Snapshot::default().authority(),
            revision: 42,
            text: "secret".into(),
            mode: "completion".into(),
            rect: None,
            render: None,
            choices: vec![],
        };
        match p.ack(String::new()) {
            Command::PresentationResult {
                revision,
                visible_text,
                ..
            } => {
                assert_eq!(revision, 42);
                assert!(visible_text.is_empty());
            }
            _ => unreachable!(),
        }
    }
}
