//! Explicit field controls, never an ordinary-completion surface.
use super::{Event, queue};
use anyhow::{Result, ensure};
use serde_json::Value;
use windows::Win32::System::SystemServices::{SS_CENTER, SS_NOTIFY};
use windows::{
    Win32::{Foundation::*, Graphics::Gdi::*, UI::WindowsAndMessaging::*},
    core::w,
};
pub struct Accessory {
    window: HWND,
    foreground: HWND,
    pub target: Option<(String, String)>,
}
unsafe extern "system" fn procedure(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if msg == WM_MOUSEACTIVATE {
        return LRESULT(MA_NOACTIVATE as isize);
    }
    if msg == WM_COMMAND || msg == WM_CONTEXTMENU {
        queue(Event::Accessory);
        return LRESULT(0);
    }
    unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
}
impl Accessory {
    pub fn new(instance: HINSTANCE) -> Result<Self> {
        unsafe {
            RegisterClassW(&WNDCLASSW {
                lpfnWndProc: Some(procedure),
                hInstance: instance,
                lpszClassName: w!("BroTypist.FieldControls"),
                hbrBackground: HBRUSH((COLOR_WINDOW.0 + 1) as *mut _),
                ..Default::default()
            });
            let window = CreateWindowExW(
                WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
                w!("BroTypist.FieldControls"),
                w!("BroTypist field controls"),
                WS_POPUP | WS_BORDER,
                0,
                0,
                34,
                26,
                None,
                None,
                Some(instance),
                None,
            )?;
            let label = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                w!("STATIC"),
                w!("B ···"),
                WS_CHILD | WS_VISIBLE | WINDOW_STYLE(SS_NOTIFY.0 | SS_CENTER.0),
                1,
                3,
                32,
                22,
                Some(window),
                Some(HMENU(1usize as *mut _)),
                Some(instance),
                None,
            )?;
            SendMessageW(
                label,
                WM_SETFONT,
                Some(WPARAM(GetStockObject(DEFAULT_GUI_FONT).0 as usize)),
                None,
            );
            Ok(Self {
                window,
                foreground: HWND::default(),
                target: None,
            })
        }
    }
    pub fn hide(&mut self) {
        unsafe {
            let _ = ShowWindow(self.window, SW_HIDE);
        }
        self.target = None;
    }
    pub fn revalidate(&mut self) {
        unsafe {
            if GetForegroundWindow() != self.foreground {
                self.hide();
            }
        }
    }
    pub fn show(&mut self, event: &Value, status: &Value) -> Result<()> {
        unsafe {
            self.hide();
            let rect = event["rect"]
                .as_array()
                .or_else(|| event["field_rect"].as_array())
                .ok_or_else(|| anyhow::anyhow!("No field control anchor"))?;
            ensure!(rect.len() == 4, "Invalid field control anchor");
            let values: Vec<_> = rect.iter().filter_map(Value::as_f64).collect();
            ensure!(
                values.len() == 4 && values.iter().all(|v| v.is_finite()),
                "Invalid field control geometry"
            );
            let app = status["current_app"].as_str().unwrap_or("");
            ensure!(
                !app.is_empty() && status["secure_input"] == false,
                "No eligible field control target"
            );
            self.foreground = GetForegroundWindow();
            let mut work = RECT::default();
            SystemParametersInfoW(
                SPI_GETWORKAREA,
                0,
                Some((&mut work as *mut RECT).cast()),
                SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
            )?;
            let x = values[0].round() as i32;
            let y = (values[1] + values[3] + 5.0).round() as i32;
            ensure!(
                x >= work.left && x + 36 <= work.right && y >= work.top && y + 28 <= work.bottom,
                "No complete field control bounds"
            );
            SetWindowPos(
                self.window,
                Some(HWND_TOPMOST),
                x,
                y,
                36,
                28,
                SWP_NOACTIVATE | SWP_SHOWWINDOW,
            )?;
            self.target = Some((
                app.into(),
                status["current_domain"].as_str().unwrap_or("").into(),
            ));
            Ok(())
        }
    }
}
impl Drop for Accessory {
    fn drop(&mut self) {
        unsafe {
            let _ = DestroyWindow(self.window);
        }
    }
}
