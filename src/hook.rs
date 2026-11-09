//! Tells the user's physical mouse and keyboard apart from software-generated input.
//!
//! On Windows low-level hooks (`WH_MOUSE_LL`, `WH_KEYBOARD_LL`) see every input
//! event together with an "injected" flag that is set for input produced by
//! `SendInput` / `mouse_event` / `keybd_event` (i.e. automation tools,
//! including AI agents' own computer-use tools). Only non-injected
//! ("hardware") input counts as the user. Other platforms are not implemented yet; `start` returns false
//! and the app falls back to plain cursor-movement detection.

/// Input observed by the hook since the previous [`take`].
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Activity {
    /// Net cursor displacement caused by hardware input.
    pub hw_dx: i32,
    pub hw_dy: i32,
    pub hw_events: u32,
    /// Hardware button presses and wheel turns.
    pub hw_buttons: u32,
    /// Physical key presses.
    pub hw_keys: u32,
    pub injected_events: u32,
}

#[cfg(windows)]
pub use win::*;

#[cfg(not(windows))]
mod fallback {
    use super::Activity;
    pub fn start() -> bool {
        false
    }
    pub fn take() -> Activity {
        Activity::default()
    }
    pub fn resync(_pos: (i32, i32)) {}
    pub fn last_button_was_injected() -> bool {
        false
    }
    pub fn totals() -> (u64, u64) {
        (0, 0)
    }
}
#[cfg(not(windows))]
pub use fallback::*;

#[cfg(windows)]
mod win {
    use super::Activity;
    use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, AtomicU64, Ordering::Relaxed};
    use std::sync::mpsc;
    use windows_sys::Win32::Foundation::{LPARAM, LRESULT, POINT, WPARAM};
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::System::Threading::{
        GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_TIME_CRITICAL,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CallNextHookEx, GetCursorPos, GetMessageW, HC_ACTION, HHOOK, KBDLLHOOKSTRUCT,
        LLKHF_INJECTED, LLKHF_LOWER_IL_INJECTED, LLMHF_INJECTED, LLMHF_LOWER_IL_INJECTED, MSG,
        MSLLHOOKSTRUCT, SetTimer, SetWindowsHookExW, UnhookWindowsHookEx, WH_KEYBOARD_LL,
        WH_MOUSE_LL, WM_KEYDOWN, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MBUTTONDOWN, WM_MBUTTONUP,
        WM_MOUSEHWHEEL, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_RBUTTONDOWN, WM_RBUTTONUP, WM_SYSKEYDOWN,
        WM_TIMER, WM_XBUTTONDOWN, WM_XBUTTONUP,
    };

    /// Single hardware move events larger than this are treated as a cursor
    /// teleport we didn't see (e.g. `SetCursorPos`), not as hand movement.
    const MAX_STEP: i32 = 400;
    /// Windows silently removes low-level hooks that time out; re-install
    /// periodically so a hiccup under heavy load can't disable detection.
    const REINSTALL_MS: u32 = 30_000;

    static HW_DX: AtomicI32 = AtomicI32::new(0);
    static HW_DY: AtomicI32 = AtomicI32::new(0);
    static HW_EVENTS: AtomicU32 = AtomicU32::new(0);
    static HW_BUTTONS: AtomicU32 = AtomicU32::new(0);
    static HW_KEYS: AtomicU32 = AtomicU32::new(0);
    static INJECTED: AtomicU32 = AtomicU32::new(0);
    static LAST_X: AtomicI32 = AtomicI32::new(0);
    static LAST_Y: AtomicI32 = AtomicI32::new(0);
    static LAST_BUTTON_INJECTED: AtomicBool = AtomicBool::new(false);
    static TOTAL_HW: AtomicU64 = AtomicU64::new(0);
    static TOTAL_INJECTED: AtomicU64 = AtomicU64::new(0);

    unsafe extern "system" fn hook_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        if code == HC_ACTION as i32 {
            // SAFETY: for WH_MOUSE_LL with HC_ACTION, lparam points to an MSLLHOOKSTRUCT.
            let info = unsafe { &*(lparam as *const MSLLHOOKSTRUCT) };
            record(wparam as u32, info.pt, info.flags);
        }
        // SAFETY: always pass the event on.
        unsafe { CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam) }
    }

    unsafe extern "system" fn key_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        if code == HC_ACTION as i32 {
            // SAFETY: for WH_KEYBOARD_LL with HC_ACTION, lparam points to a KBDLLHOOKSTRUCT.
            let info = unsafe { &*(lparam as *const KBDLLHOOKSTRUCT) };
            record_key(wparam as u32, info.flags);
        }
        // SAFETY: always pass the event on.
        unsafe { CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam) }
    }

    fn record_key(msg: u32, flags: u32) {
        if flags & (LLKHF_INJECTED | LLKHF_LOWER_IL_INJECTED) != 0 {
            INJECTED.fetch_add(1, Relaxed);
            TOTAL_INJECTED.fetch_add(1, Relaxed);
            return;
        }
        HW_EVENTS.fetch_add(1, Relaxed);
        TOTAL_HW.fetch_add(1, Relaxed);
        if matches!(msg, WM_KEYDOWN | WM_SYSKEYDOWN) {
            HW_KEYS.fetch_add(1, Relaxed);
        }
    }

    /// Must stay tiny: it runs synchronously for every mouse event system-wide.
    fn record(msg: u32, pt: POINT, flags: u32) {
        let injected = flags & (LLMHF_INJECTED | LLMHF_LOWER_IL_INJECTED) != 0;
        let lx = LAST_X.swap(pt.x, Relaxed);
        let ly = LAST_Y.swap(pt.y, Relaxed);
        let press_or_wheel = matches!(
            msg,
            WM_LBUTTONDOWN
                | WM_RBUTTONDOWN
                | WM_MBUTTONDOWN
                | WM_XBUTTONDOWN
                | WM_MOUSEWHEEL
                | WM_MOUSEHWHEEL
        );
        let button = press_or_wheel
            || matches!(
                msg,
                WM_LBUTTONUP | WM_RBUTTONUP | WM_MBUTTONUP | WM_XBUTTONUP
            );
        if button {
            LAST_BUTTON_INJECTED.store(injected, Relaxed);
        }
        if injected {
            INJECTED.fetch_add(1, Relaxed);
            TOTAL_INJECTED.fetch_add(1, Relaxed);
            return;
        }
        HW_EVENTS.fetch_add(1, Relaxed);
        TOTAL_HW.fetch_add(1, Relaxed);
        if msg == WM_MOUSEMOVE {
            let (dx, dy) = (pt.x - lx, pt.y - ly);
            if dx.abs() < MAX_STEP && dy.abs() < MAX_STEP {
                HW_DX.fetch_add(dx, Relaxed);
                HW_DY.fetch_add(dy, Relaxed);
            }
        }
        if press_or_wheel {
            HW_BUTTONS.fetch_add(1, Relaxed);
        }
    }

    fn install_mouse() -> HHOOK {
        // SAFETY: plain Win32 calls; the hook procedure is a 'static fn.
        unsafe {
            SetWindowsHookExW(
                WH_MOUSE_LL,
                Some(hook_proc),
                GetModuleHandleW(std::ptr::null()),
                0,
            )
        }
    }

    fn install_keyboard() -> HHOOK {
        // SAFETY: as above.
        unsafe {
            SetWindowsHookExW(
                WH_KEYBOARD_LL,
                Some(key_proc),
                GetModuleHandleW(std::ptr::null()),
                0,
            )
        }
    }

    /// Install the new hook before removing the old one so no event is missed.
    fn reinstall(hook: &mut HHOOK, install: fn() -> HHOOK) {
        let fresh = install();
        if !fresh.is_null() {
            if !hook.is_null() {
                // SAFETY: `hook` came from SetWindowsHookExW on this thread.
                unsafe { UnhookWindowsHookEx(*hook) };
            }
            *hook = fresh;
        }
    }

    /// Install the hook on its own thread. Returns whether it is active.
    pub fn start() -> bool {
        let (tx, rx) = mpsc::channel();
        let spawned = std::thread::Builder::new()
            .name("mouse-hook".into())
            .spawn(move || {
                // SAFETY: Win32 calls on this thread only.
                unsafe {
                    SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_TIME_CRITICAL);
                    let mut p = POINT { x: 0, y: 0 };
                    if GetCursorPos(&mut p) != 0 {
                        resync((p.x, p.y));
                    }
                    let mut hook = install_mouse();
                    let mut key_hook = install_keyboard();
                    let _ = tx.send(!hook.is_null());
                    if hook.is_null() {
                        return;
                    }
                    SetTimer(std::ptr::null_mut(), 0, REINSTALL_MS, None);
                    let mut msg: MSG = std::mem::zeroed();
                    // The hook is serviced while this thread pumps messages.
                    while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
                        if msg.message == WM_TIMER {
                            reinstall(&mut hook, install_mouse);
                            reinstall(&mut key_hook, install_keyboard);
                        }
                    }
                    UnhookWindowsHookEx(hook);
                    if !key_hook.is_null() {
                        UnhookWindowsHookEx(key_hook);
                    }
                }
            });
        spawned.is_ok() && rx.recv().unwrap_or(false)
    }

    pub fn take() -> Activity {
        Activity {
            hw_dx: HW_DX.swap(0, Relaxed),
            hw_dy: HW_DY.swap(0, Relaxed),
            hw_events: HW_EVENTS.swap(0, Relaxed),
            hw_buttons: HW_BUTTONS.swap(0, Relaxed),
            hw_keys: HW_KEYS.swap(0, Relaxed),
            injected_events: INJECTED.swap(0, Relaxed),
        }
    }

    /// Tell the hook where the cursor really is (after a move it couldn't see).
    pub fn resync(pos: (i32, i32)) {
        LAST_X.store(pos.0, Relaxed);
        LAST_Y.store(pos.1, Relaxed);
    }

    /// Whether the most recent button press/release was software-generated.
    pub fn last_button_was_injected() -> bool {
        LAST_BUTTON_INJECTED.load(Relaxed)
    }

    /// (hardware events, injected events) since start.
    pub fn totals() -> (u64, u64) {
        (TOTAL_HW.load(Relaxed), TOTAL_INJECTED.load(Relaxed))
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use windows_sys::Win32::UI::WindowsAndMessaging::WM_KEYUP;

        // One test only: the counters are process-global.
        #[test]
        fn classifies_events() {
            let _ = take();
            resync((100, 100));
            let p = |x, y| POINT { x, y };
            record(WM_MOUSEMOVE, p(103, 98), 0); // hardware
            record(WM_MOUSEMOVE, p(900, 900), LLMHF_INJECTED); // agent
            record(WM_MOUSEMOVE, p(905, 900), 0); // hardware nudge after the jump
            record(WM_MOUSEMOVE, p(2000, 100), 0); // unseen teleport: ignored
            record(WM_LBUTTONDOWN, p(2000, 100), LLMHF_INJECTED);
            assert!(last_button_was_injected());
            record(WM_LBUTTONUP, p(2000, 100), 0);
            assert!(!last_button_was_injected());
            record(WM_MOUSEWHEEL, p(2000, 100), 0);
            record_key(WM_KEYDOWN, 0); // physical key
            record_key(WM_KEYUP, 0);
            record_key(WM_KEYDOWN, LLKHF_INJECTED); // agent typing
            let a = take();
            assert_eq!((a.hw_dx, a.hw_dy), (3 + 5, -2));
            assert_eq!(a.hw_events, 7);
            assert_eq!(a.hw_buttons, 1);
            assert_eq!(a.hw_keys, 1);
            assert_eq!(a.injected_events, 3);
            assert_eq!(take(), Activity::default());
        }
    }
}
