//! Windows-only fixes for the bell's borderless, transparent window, done by
//! subclassing winit's window procedure.
//!
//! * egui-winit always requests winit's "undecorated shadow". winit implements
//!   it by keeping a sliver of non-client area, so DWM draws a border, rounded
//!   corners and a drop shadow, and a 1px line shows at the top. Handling
//!   `WM_NCCALCSIZE` ourselves makes the client area cover the whole window,
//!   which removes all of them.
//! * Mouse passthrough is implemented by winit as adding/removing
//!   `WS_EX_TRANSPARENT | WS_EX_LAYERED`. Toggling `WS_EX_LAYERED` makes the
//!   compositor rebuild the window surface, which can flash the whole window.
//!   Once winit has made the window layered we keep it that way, so only
//!   `WS_EX_TRANSPARENT` (which controls click-through) changes. Note: setting
//!   `WS_EX_LAYERED` ourselves before the first frame leaves the window
//!   invisible, so we only preserve it.
//! * The saved position is in physical pixels: logical points depend on the
//!   scale of the monitor, and winit converts them with the primary monitor's
//!   scale, which put the bell off screen on mixed-DPI setups.

use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use std::sync::atomic::{AtomicIsize, Ordering};
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{MONITOR_DEFAULTTONULL, MonitorFromPoint};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CallWindowProcW, GWL_EXSTYLE, GWLP_WNDPROC, GetWindowRect, STYLESTRUCT, SWP_FRAMECHANGED,
    SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, SetWindowPos, WM_NCCALCSIZE,
    WM_STYLECHANGING, WNDPROC, WS_EX_LAYERED,
};

#[cfg(target_pointer_width = "64")]
use windows_sys::Win32::UI::WindowsAndMessaging::SetWindowLongPtrW as set_long;
#[cfg(target_pointer_width = "32")]
use windows_sys::Win32::UI::WindowsAndMessaging::SetWindowLongW as set_long;

/// The window procedure we replaced (winit's).
static ORIGINAL_PROC: AtomicIsize = AtomicIsize::new(0);

/// Installs the fixes and moves the window to `pos` (physical pixels). Returns
/// whether it was placed; not if there's no `pos` or the point at `anchor`
/// (fraction of the window size, i.e. the bell) would be on no monitor.
pub fn apply(cc: &eframe::CreationContext<'_>, pos: Option<[f32; 2]>, anchor: [f32; 2]) -> bool {
    let Ok(handle) = cc.window_handle() else {
        return false;
    };
    let RawWindowHandle::Win32(h) = handle.as_raw() else {
        return false;
    };
    let hwnd = h.hwnd.get() as HWND;
    let placed = pos.is_some_and(|[x, y]| unsafe { place(hwnd, x as i32, y as i32, anchor) });
    unsafe {
        let prev = set_long(hwnd, GWLP_WNDPROC, wnd_proc as *const () as _);
        if prev == 0 {
            return placed;
        }
        ORIGINAL_PROC.store(prev as isize, Ordering::SeqCst);
        // Re-run WM_NCCALCSIZE through our handler.
        SetWindowPos(
            hwnd,
            std::ptr::null_mut(),
            0,
            0,
            0,
            0,
            SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
        );
    }
    placed
}

unsafe fn place(hwnd: HWND, x: i32, y: i32, anchor: [f32; 2]) -> bool {
    let mut r: RECT = unsafe { std::mem::zeroed() };
    if unsafe { GetWindowRect(hwnd, &mut r) } == 0 {
        return false;
    }
    let pt = POINT {
        x: x + ((r.right - r.left) as f32 * anchor[0]) as i32,
        y: y + ((r.bottom - r.top) as f32 * anchor[1]) as i32,
    };
    if unsafe { MonitorFromPoint(pt, MONITOR_DEFAULTTONULL) }.is_null() {
        return false;
    }
    let flags = SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE;
    unsafe { SetWindowPos(hwnd, std::ptr::null_mut(), x, y, 0, 0, flags) != 0 }
}

unsafe extern "system" fn wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        // Client area = whole window: no non-client frame at all.
        WM_NCCALCSIZE if wparam != 0 => return 0,
        WM_STYLECHANGING if wparam as i32 == GWL_EXSTYLE => {
            let style = unsafe { &mut *(lparam as *mut STYLESTRUCT) };
            if style.styleOld & WS_EX_LAYERED != 0 {
                style.styleNew |= WS_EX_LAYERED;
            }
        }
        _ => {}
    }
    let original: WNDPROC = unsafe { std::mem::transmute(ORIGINAL_PROC.load(Ordering::SeqCst)) };
    unsafe { CallWindowProcW(original, hwnd, msg, wparam, lparam) }
}
