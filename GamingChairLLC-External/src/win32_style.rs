//! Windows-only window cosmetics: dark title bar and taskbar/window icon.
//! Both are best-effort; failures are logged and ignored.

use std::ffi::c_void;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;

use tao::platform::windows::WindowExtWindows;
use tao::window::Window;
use windows_sys::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows_sys::Win32::Graphics::Dwm::DwmSetWindowAttribute;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GetSystemMetrics, LoadImageW, SendMessageW, ICON_BIG, ICON_SMALL, IMAGE_ICON, LR_LOADFROMFILE,
    SM_CXICON, SM_CXSMICON, SM_CYICON, SM_CYSMICON, WM_SETICON,
};

const DWMWA_USE_IMMERSIVE_DARK_MODE: u32 = 20;
const DWMWA_BORDER_COLOR: u32 = 34;
const DWMWA_CAPTION_COLOR: u32 = 35;
const DWMWA_TEXT_COLOR: u32 = 36;

// COLORREF is 0x00BBGGRR. #0e0e11 -> 0x00110E0E.
const CAPTION_COLOR: u32 = 0x0011_0E0E;
const TEXT_COLOR: u32 = 0x00FF_FFFF;

pub fn apply(window: &Window, icon: &Path) {
    let hwnd = window.hwnd() as HWND;
    if hwnd.is_null() {
        log::warn!(target: "GamingChairLLC.win32", "Window has no HWND");
        return;
    }

    apply_style(hwnd);
    apply_icon(hwnd, icon);
}

fn apply_style(hwnd: HWND) {
    unsafe {
        set_attribute(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, 1i32);
        set_attribute(hwnd, DWMWA_CAPTION_COLOR, CAPTION_COLOR);
        set_attribute(hwnd, DWMWA_BORDER_COLOR, CAPTION_COLOR);
        set_attribute(hwnd, DWMWA_TEXT_COLOR, TEXT_COLOR);
    }

    log::info!(target: "GamingChairLLC.win32", "Applied Windows window styling");
}

unsafe fn set_attribute<T>(hwnd: HWND, attribute: u32, value: T) {
    let result = DwmSetWindowAttribute(
        hwnd,
        attribute,
        &value as *const T as *const c_void,
        std::mem::size_of::<T>() as u32,
    );

    if result != 0 {
        log::debug!(
            target: "GamingChairLLC.win32",
            "DwmSetWindowAttribute({attribute}) returned 0x{result:08x}"
        );
    }
}

fn apply_icon(hwnd: HWND, icon: &Path) {
    if !icon.is_file() {
        log::warn!(
            target: "GamingChairLLC.win32",
            "Window icon not found: {}",
            icon.display()
        );
        return;
    }

    let wide: Vec<u16> = icon
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    unsafe {
        let small = load_icon(&wide, SM_CXSMICON, SM_CYSMICON);
        let large = load_icon(&wide, SM_CXICON, SM_CYICON);

        if small.is_null() || large.is_null() {
            log::warn!(
                target: "GamingChairLLC.win32",
                "Failed to load window icon: {}",
                icon.display()
            );
            return;
        }

        SendMessageW(hwnd, WM_SETICON, ICON_SMALL as WPARAM, small as LPARAM);
        SendMessageW(hwnd, WM_SETICON, ICON_BIG as WPARAM, large as LPARAM);
    }

    log::info!(
        target: "GamingChairLLC.win32",
        "Applied Windows window icon: {}",
        icon.display()
    );
}

unsafe fn load_icon(
    wide_path: &[u16],
    width_metric: i32,
    height_metric: i32,
) -> *mut c_void {
    let width = GetSystemMetrics(width_metric);
    let height = GetSystemMetrics(height_metric);

    LoadImageW(
        std::ptr::null_mut(),
        wide_path.as_ptr(),
        IMAGE_ICON,
        width,
        height,
        LR_LOADFROMFILE,
    )
}
