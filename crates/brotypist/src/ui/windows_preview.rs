//! Ephemeral owned Edit renderer: native font/caret, no external host authority.
use anyhow::{Result, ensure};
use std::cell::{Cell, RefCell};
use windows::Win32::{
    Foundation::*,
    Graphics::Gdi::*,
    UI::{Controls::*, Input::KeyboardAndMouse::GetFocus, Shell::*, WindowsAndMessaging::*},
};
struct Ghost {
    proposal: RefCell<String>,
    visible: RefCell<String>,
    composing: Cell<bool>,
}
pub struct PreviewGhost {
    edit: HWND,
    state: Box<Ghost>,
}
unsafe extern "system" fn procedure(
    hwnd: HWND,
    msg: u32,
    wp: WPARAM,
    lp: LPARAM,
    _id: usize,
    data: usize,
) -> LRESULT {
    unsafe {
        let Some(state) = (data as *const Ghost).as_ref() else {
            return DefSubclassProc(hwnd, msg, wp, lp);
        };
        match msg {
            WM_IME_STARTCOMPOSITION => {
                state.composing.set(true);
                state.visible.borrow_mut().clear();
                state.proposal.borrow_mut().clear();
                let _ = InvalidateRect(Some(hwnd), None, true);
            }
            WM_IME_ENDCOMPOSITION => state.composing.set(false),
            WM_KILLFOCUS | WM_LBUTTONDOWN | WM_KEYDOWN | WM_MOUSEWHEEL | WM_VSCROLL
            | WM_HSCROLL => {
                state.visible.borrow_mut().clear();
                state.proposal.borrow_mut().clear();
                let _ = InvalidateRect(Some(hwnd), None, true);
            }
            _ => {}
        }
        let result = DefSubclassProc(hwnd, msg, wp, lp);
        if msg == WM_PAINT {
            paint(hwnd, state, None);
        } else if msg == WM_PRINTCLIENT {
            paint(hwnd, state, Some(HDC(wp.0 as *mut _)));
        }
        result
    }
}
unsafe fn paint(edit: HWND, state: &Ghost, print_dc: Option<HDC>) {
    unsafe {
        state.visible.borrow_mut().clear();
        if state.composing.get() || GetFocus() != edit || !IsWindowVisible(edit).as_bool() {
            return;
        }
        let proposal = state.proposal.borrow();
        if proposal.is_empty() {
            return;
        }
        let mut rect = RECT::default();
        SendMessageW(
            edit,
            EM_GETRECT,
            None,
            Some(LPARAM((&mut rect as *mut RECT) as isize)),
        );
        let mut caret = POINT::default();
        if GetCaretPos(&mut caret).is_err() {
            return;
        }
        let mut start = 0u32;
        let mut end = 0u32;
        SendMessageW(
            edit,
            EM_GETSEL,
            Some(WPARAM((&mut start as *mut u32) as usize)),
            Some(LPARAM((&mut end as *mut u32) as isize)),
        );
        if start != end || caret.x < rect.left || caret.y < rect.top {
            return;
        }
        let text = super::text(edit);
        let source: Vec<_> = text.encode_utf16().collect();
        let row = SendMessageW(edit, EM_LINEFROMCHAR, Some(WPARAM(start as usize)), None).0;
        let row_start = SendMessageW(edit, EM_LINEINDEX, Some(WPARAM(row.max(0) as usize)), None).0;
        let row_length = SendMessageW(edit, EM_LINELENGTH, Some(WPARAM(start as usize)), None).0;
        if row < 0 || row_start < 0 || row_length < 0 {
            return;
        }
        let row_end = (row_start as usize).saturating_add(row_length as usize);
        let Some(suffix) = source.get(start as usize..row_end) else {
            return;
        };
        let Ok(suffix_text) = String::from_utf16(suffix) else {
            return;
        };
        if suffix_text.chars().any(char::is_control) {
            return;
        }
        let font = HFONT(SendMessageW(edit, WM_GETFONT, None, None).0 as *mut _);
        if font.0.is_null() {
            return;
        }
        let dc = print_dc.unwrap_or_else(|| GetDC(Some(edit)));
        if dc.0.is_null() {
            return;
        }
        let old = SelectObject(dc, font.into());
        let mut metrics = TEXTMETRICW::default();
        if GetTextMetricsW(dc, &mut metrics).as_bool() && caret.y + metrics.tmHeight <= rect.bottom
        {
            let mut suffix_size = SIZE::default();
            let suffix_ok =
                suffix.is_empty() || GetTextExtentPoint32W(dc, suffix, &mut suffix_size).as_bool();
            if suffix_ok && caret.x + suffix_size.cx <= rect.right {
                if let Some((visible, units, size)) =
                    super::overlay::fit_text(dc, &proposal, rect.right - caret.x - suffix_size.cx)
                {
                    // This field is ours: its exact current row, font, palette and suffix are known.
                    // Repaint only that row; the document and next native line remain untouched.
                    FillRect(
                        dc,
                        &RECT {
                            left: caret.x,
                            top: caret.y,
                            right: rect.right,
                            bottom: caret.y + metrics.tmHeight,
                        },
                        GetSysColorBrush(COLOR_WINDOW),
                    );
                    SetBkMode(dc, TRANSPARENT);
                    SetTextColor(dc, COLORREF(GetSysColor(COLOR_GRAYTEXT)));
                    let drawn = TextOutW(dc, caret.x, caret.y, &units).as_bool();
                    SetTextColor(dc, COLORREF(GetSysColor(COLOR_WINDOWTEXT)));
                    let suffix_drawn = suffix.is_empty()
                        || TextOutW(dc, caret.x + size.cx, caret.y, suffix).as_bool();
                    if drawn && suffix_drawn {
                        *state.visible.borrow_mut() = visible;
                    }
                }
            }
        }
        let _ = SelectObject(dc, old);
        if print_dc.is_none() {
            ReleaseDC(Some(edit), dc);
        }
    }
}
impl PreviewGhost {
    pub fn new(edit: HWND) -> Result<Self> {
        let state = Box::new(Ghost {
            proposal: RefCell::new(String::new()),
            visible: RefCell::new(String::new()),
            composing: Cell::new(false),
        });
        ensure!(
            unsafe {
                SetWindowSubclass(edit, Some(procedure), 1, (&*state as *const Ghost) as usize)
                    .as_bool()
            },
            "The owned preview Edit could not attach its native renderer."
        );
        Ok(Self { edit, state })
    }
    pub fn show(&self, text: &str) {
        if self.state.proposal.borrow().as_str() == text {
            return;
        }
        *self.state.proposal.borrow_mut() = text.into();
        self.state.visible.borrow_mut().clear();
        unsafe {
            let _ = InvalidateRect(Some(self.edit), None, true);
            let _ = UpdateWindow(self.edit);
        }
    }
    pub fn visible(&self) -> String {
        self.state.visible.borrow().clone()
    }
    pub fn hide(&self) {
        self.show("");
    }
}
impl Drop for PreviewGhost {
    fn drop(&mut self) {
        unsafe {
            let _ = RemoveWindowSubclass(self.edit, Some(procedure), 1);
        }
    }
}
