//! Keyboard capture on Windows.
//!
//! GTK cannot inhibit system shortcuts on Windows, so Win, Alt+Tab, Ctrl+Esc
//! and friends would act on the local desktop. While a session view has focus
//! and captures the keyboard, a low-level keyboard hook (as in mstsc) sends
//! every key to the remote side with its hardware scancode and swallows it.
//!
//! Caps/Num/Scroll Lock also reach the local system, so the local lock state
//! follows the remote one. Swallowed keys never update the system key state,
//! so the client's own Ctrl+Alt shortcuts are recognised here, from the keys
//! held on the remote side.

use std::cell::RefCell;
use std::ffi::c_void;

use gtk::glib;
use gtk::prelude::*;
use rdp_core::keymap;

use crate::session_view::{HostAction, SessionView};

type Hhook = *mut c_void;
type Hwnd = *mut c_void;
type HookProc = unsafe extern "system" fn(i32, usize, isize) -> isize;

#[repr(C)]
#[allow(non_snake_case)]
struct KBDLLHOOKSTRUCT {
    vkCode: u32,
    scanCode: u32,
    flags: u32,
    time: u32,
    dwExtraInfo: usize,
}

#[link(name = "user32")]
unsafe extern "system" {
    fn SetWindowsHookExW(id: i32, proc_: HookProc, module: *mut c_void, thread: u32) -> Hhook;
    fn UnhookWindowsHookEx(hook: Hhook) -> i32;
    fn CallNextHookEx(hook: Hhook, code: i32, wparam: usize, lparam: isize) -> isize;
    fn GetForegroundWindow() -> Hwnd;
    fn GetActiveWindow() -> Hwnd;
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetModuleHandleW(name: *const u16) -> *mut c_void;
}

const WH_KEYBOARD_LL: i32 = 13;
const HC_ACTION: i32 = 0;
const WM_KEYDOWN: usize = 0x0100;
const WM_SYSKEYDOWN: usize = 0x0104;
const LLKHF_EXTENDED: u32 = 0x01;

const VK_RETURN: u32 = 0x0D;
const VK_PAUSE: u32 = 0x13;
const VK_CAPITAL: u32 = 0x14;
const VK_END: u32 = 0x23;
const VK_HOME: u32 = 0x24;
const VK_NUMLOCK: u32 = 0x90;
const VK_SCROLL: u32 = 0x91;
const VK_RSHIFT: u32 = 0xA1;

struct Capture {
    hook: Hhook,
    view: glib::WeakRef<SessionView>,
}

thread_local! {
    // The hook procedure runs on the thread that installed it (the GTK one).
    static CAPTURE: RefCell<Option<Capture>> = const { RefCell::new(None) };
}

/// Start capturing the keyboard for `view`.
pub fn activate(view: &SessionView) {
    CAPTURE.with(|c| {
        let mut c = c.borrow_mut();
        if let Some(cap) = c.as_mut() {
            cap.view = view.downgrade();
            return;
        }
        // SAFETY: a low-level hook needs the module handle of this executable.
        let hook = unsafe {
            SetWindowsHookExW(WH_KEYBOARD_LL, hook_proc, GetModuleHandleW(std::ptr::null()), 0)
        };
        if hook.is_null() {
            tracing::warn!(
                "keyboard hook: {}, system shortcuts stay local",
                std::io::Error::last_os_error()
            );
            return;
        }
        *c = Some(Capture {
            hook,
            view: view.downgrade(),
        });
    });
}

/// Stop capturing, if `view` is the one being captured for.
pub fn deactivate(view: &SessionView) {
    CAPTURE.with(|c| {
        let mut c = c.borrow_mut();
        let ours = c
            .as_ref()
            .is_some_and(|cap| cap.view.upgrade().is_none_or(|v| &v == view));
        if ours && let Some(cap) = c.take() {
            // SAFETY: the handle came from SetWindowsHookExW.
            unsafe { UnhookWindowsHookEx(cap.hook) };
        }
    });
}

/// Whether keys currently go through the hook rather than GTK.
pub fn is_active() -> bool {
    CAPTURE.with(|c| c.borrow().is_some())
}

/// RDP scancode for a hooked key, or None if it has no usable scancode.
fn scancode(kb: &KBDLLHOOKSTRUCT) -> Option<u32> {
    match kb.vkCode {
        VK_PAUSE => return Some(keymap::PAUSE),
        // Both report the extended flag, but RDP wants the plain codes.
        VK_NUMLOCK => return Some(0x45),
        VK_RSHIFT => return Some(0x36),
        _ => {}
    }
    let sc = kb.scanCode & 0xFF;
    if sc == 0 {
        // Synthetic keys (e.g. some media keys) may come without a scancode.
        return keymap::vk_to_rdp(kb.vkCode);
    }
    Some(if kb.flags & LLKHF_EXTENDED != 0 {
        keymap::EXTENDED | sc
    } else {
        sc
    })
}

/// Handle a key event; true means it was sent to the remote side and must
/// not reach anything local.
fn handle(wparam: usize, kb: &KBDLLHOOKSTRUCT) -> bool {
    let Some(view) = CAPTURE.with(|c| c.borrow().as_ref().and_then(|cap| cap.view.upgrade()))
    else {
        return false;
    };
    // Only while one of our windows is in front.
    // SAFETY: plain Win32 calls.
    let front = unsafe { GetForegroundWindow() };
    if front.is_null() || front != unsafe { GetActiveWindow() } {
        return false;
    }

    let down = matches!(wparam, WM_KEYDOWN | WM_SYSKEYDOWN);
    let vk = kb.vkCode;
    let ctrl = view.key_held(keymap::LEFT_CTRL) || view.key_held(keymap::EXTENDED | 0x1D);
    let alt = view.key_held(keymap::LEFT_ALT) || view.key_held(keymap::EXTENDED | 0x38);
    if ctrl && alt {
        let action = match vk {
            VK_RETURN => Some(HostAction::ToggleFullscreen),
            VK_END => Some(HostAction::CtrlAltDel),
            VK_HOME => Some(HostAction::ToggleGrab),
            _ => None,
        };
        if let Some(action) = action {
            if down {
                // The handler may stop the capture: run it outside the hook.
                glib::idle_add_local_once(move || view.host_action(action));
            }
            return true;
        }
    }
    let Some(sc) = scancode(kb) else {
        return false;
    };
    view.send_scancode(sc, down);
    !matches!(vk, VK_CAPITAL | VK_NUMLOCK | VK_SCROLL)
}

unsafe extern "system" fn hook_proc(code: i32, wparam: usize, lparam: isize) -> isize {
    if code == HC_ACTION {
        // SAFETY: for HC_ACTION, lparam points to a KBDLLHOOKSTRUCT.
        let kb = unsafe { &*(lparam as *const KBDLLHOOKSTRUCT) };
        if handle(wparam, kb) {
            return 1;
        }
    }
    // SAFETY: forwarding the unmodified arguments.
    unsafe { CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam) }
}
