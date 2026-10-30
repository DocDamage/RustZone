//! Native window that hosts a fallback VST3 plugin's editor (plugin editors need a real HWND).
//! Closing it only hides it; it is destroyed when the plugin is unloaded.

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    AdjustWindowRectEx, CreateWindowExW, DefWindowProcW, DestroyWindow, RegisterClassExW, SetWindowPos,
    ShowWindow, CS_HREDRAW, CS_VREDRAW, CW_USEDEFAULT, HCURSOR, HICON, SWP_NOMOVE, SWP_NOZORDER, SW_HIDE, SW_SHOW,
    WINDOW_EX_STYLE, WM_CLOSE, WNDCLASSEXW, WS_CAPTION, WS_CLIPCHILDREN, WS_MINIMIZEBOX, WS_OVERLAPPED, WS_SYSMENU,
};
use windows::Win32::Graphics::Gdi::HBRUSH;
use windows::Win32::Foundation::HINSTANCE;

use crate::vst3::Vst3Plugin;

const CLASS: PCWSTR = w!("MasteringStudioPluginEditor");

unsafe extern "system" fn host_wnd_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if msg == WM_CLOSE {
        let _ = ShowWindow(hwnd, SW_HIDE);
        return LRESULT(0);
    }
    DefWindowProcW(hwnd, msg, wparam, lparam)
}

pub struct PluginWindow {
    hwnd: HWND,
}

impl PluginWindow {
    /// Create a window owned by `owner`, attach the plugin's editor and show it
    pub fn open(owner: Option<HWND>, plugin: &Vst3Plugin) -> Result<Self, String> {
        unsafe {
            let wc = WNDCLASSEXW {
                cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
                style: CS_HREDRAW | CS_VREDRAW,
                lpfnWndProc: Some(host_wnd_proc),
                hInstance: HINSTANCE(std::ptr::null_mut()),
                hIcon: HICON(std::ptr::null_mut()),
                hCursor: HCURSOR(std::ptr::null_mut()),
                hbrBackground: HBRUSH(std::ptr::null_mut()),
                lpszClassName: CLASS,
                ..Default::default()
            };
            // Registering twice fails harmlessly
            let _ = RegisterClassExW(&wc);

            let title: Vec<u16> = format!("{} (fallback plugin)", plugin.name).encode_utf16().chain(Some(0)).collect();
            let style = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX | WS_CLIPCHILDREN;
            let hwnd = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                CLASS,
                PCWSTR(title.as_ptr()),
                style,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                800,
                600,
                owner.unwrap_or_default(),
                None,
                None,
                None,
            )
            .map_err(|e| format!("Could not create the plugin window: {}", e))?;

            match plugin.create_view(hwnd) {
                Ok(rect) => {
                    let mut r = RECT { left: 0, top: 0, right: rect.width(), bottom: rect.height() };
                    let _ = AdjustWindowRectEx(&mut r, style, false, WINDOW_EX_STYLE(0));
                    let _ = SetWindowPos(hwnd, None, 0, 0, r.right - r.left, r.bottom - r.top, SWP_NOZORDER | SWP_NOMOVE);
                }
                Err(e) => {
                    let _ = DestroyWindow(hwnd);
                    return Err(e);
                }
            }
            let _ = ShowWindow(hwnd, SW_SHOW);
            Ok(Self { hwnd })
        }
    }

    pub fn show(&self) {
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_SHOW);
        }
    }

    /// Detach the editor and destroy the window
    pub fn close(self, plugin: Option<&Vst3Plugin>) {
        if let Some(p) = plugin {
            p.destroy_view();
        }
        unsafe {
            let _ = DestroyWindow(self.hwnd);
        }
    }
}
