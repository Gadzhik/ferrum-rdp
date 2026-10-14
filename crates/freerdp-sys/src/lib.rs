//! Raw bindings to the ferrum C shim (`shim/ferrum_shim.h`).
//!
//! The shim hides all FreeRDP structures; keep this file in sync with the header.
#![allow(non_camel_case_types)]

use std::ffi::{c_char, c_int, c_void};

#[repr(C)]
pub struct ferrum_session {
    _private: [u8; 0],
}

pub const FERRUM_STATE_CONNECTING: c_int = 0;
pub const FERRUM_STATE_CONNECTED: c_int = 1;
pub const FERRUM_STATE_RECONNECTING: c_int = 2;
pub const FERRUM_STATE_DISCONNECTED: c_int = 3;

pub const FERRUM_CURSOR_BITMAP: c_int = 0;
pub const FERRUM_CURSOR_HIDDEN: c_int = 1;
pub const FERRUM_CURSOR_DEFAULT: c_int = 2;

pub const FERRUM_CERT_REJECT: c_int = 0;
pub const FERRUM_CERT_ACCEPT_PERMANENT: c_int = 1;
pub const FERRUM_CERT_ACCEPT_ONCE: c_int = 2;

pub const VERIFY_CERT_FLAG_LEGACY: u32 = 0x02;
pub const VERIFY_CERT_FLAG_REDIRECT: u32 = 0x10;
pub const VERIFY_CERT_FLAG_GATEWAY: u32 = 0x20;
pub const VERIFY_CERT_FLAG_CHANGED: u32 = 0x40;
pub const VERIFY_CERT_FLAG_MISMATCH: u32 = 0x80;

pub const FERRUM_RAIL_TITLE: u32 = 1 << 0;
pub const FERRUM_RAIL_OFFSET: u32 = 1 << 1;
pub const FERRUM_RAIL_SIZE: u32 = 1 << 2;
pub const FERRUM_RAIL_STATE: u32 = 1 << 3;
pub const FERRUM_RAIL_STYLE: u32 = 1 << 4;
pub const FERRUM_RAIL_OWNER: u32 = 1 << 5;
pub const FERRUM_RAIL_NEW: u32 = 1 << 6;

#[repr(C)]
pub struct ferrum_cert_info {
    pub host: *const c_char,
    pub port: u16,
    pub common_name: *const c_char,
    pub subject: *const c_char,
    pub issuer: *const c_char,
    pub fingerprint: *const c_char,
    pub old_subject: *const c_char,
    pub old_issuer: *const c_char,
    pub old_fingerprint: *const c_char,
    pub flags: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct ferrum_monitor {
    pub id: u32,
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    pub primary: bool,
    pub physical_width_mm: u32,
    pub physical_height_mm: u32,
    pub orientation: u32,
    pub desktop_scale: u32,
    pub device_scale: u32,
}

#[repr(C)]
pub struct ferrum_rail_window {
    pub window_id: u64,
    pub owner_id: u64,
    pub fields: u32,
    pub title: *const c_char,
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    pub show_state: u32,
    pub style: u32,
    pub ex_style: u32,
}

#[repr(C)]
pub struct ferrum_callbacks {
    pub user: *mut c_void,

    pub on_state: Option<unsafe extern "C" fn(user: *mut c_void, state: c_int)>,
    pub on_desktop_resize: Option<unsafe extern "C" fn(user: *mut c_void, width: u32, height: u32)>,
    pub on_frame: Option<
        unsafe extern "C" fn(
            user: *mut c_void,
            data: *const u8,
            stride: u32,
            width: u32,
            height: u32,
            rects: *const i32,
            nrects: usize,
        ),
    >,
    pub on_cursor: Option<
        unsafe extern "C" fn(
            user: *mut c_void,
            kind: c_int,
            bgra: *const u8,
            width: u32,
            height: u32,
            hot_x: u32,
            hot_y: u32,
        ),
    >,
    pub on_cursor_position: Option<unsafe extern "C" fn(user: *mut c_void, x: u32, y: u32)>,

    pub authenticate: Option<
        unsafe extern "C" fn(
            user: *mut c_void,
            reason: c_int,
            username: *mut *mut c_char,
            password: *mut *mut c_char,
            domain: *mut *mut c_char,
        ) -> bool,
    >,
    pub verify_certificate:
        Option<unsafe extern "C" fn(user: *mut c_void, info: *const ferrum_cert_info) -> c_int>,
    pub gateway_message: Option<
        unsafe extern "C" fn(
            user: *mut c_void,
            kind: u32,
            display_mandatory: bool,
            consent_mandatory: bool,
            message: *const c_char,
        ) -> bool,
    >,
    pub choose_smartcard: Option<
        unsafe extern "C" fn(user: *mut c_void, names: *const *const c_char, count: usize) -> c_int,
    >,
    pub logon_error:
        Option<unsafe extern "C" fn(user: *mut c_void, data: *const c_char, kind: *const c_char)>,

    pub clip_remote_offer:
        Option<unsafe extern "C" fn(user: *mut c_void, mimes: *const *const c_char, count: usize)>,
    pub clip_remote_data: Option<
        unsafe extern "C" fn(user: *mut c_void, request_id: u64, data: *const u8, len: usize),
    >,
    pub clip_local_get: Option<
        unsafe extern "C" fn(user: *mut c_void, mime: *const c_char, len: *mut usize) -> *mut u8,
    >,

    pub rail_window:
        Option<unsafe extern "C" fn(user: *mut c_void, window: *const ferrum_rail_window)>,
    pub rail_window_delete: Option<unsafe extern "C" fn(user: *mut c_void, window_id: u64)>,
    pub rail_window_icon: Option<
        unsafe extern "C" fn(
            user: *mut c_void,
            window_id: u64,
            bgra: *const u8,
            width: u32,
            height: u32,
        ),
    >,
    pub rail_exec_result: Option<
        unsafe extern "C" fn(
            user: *mut c_void,
            exec_result: u32,
            raw_result: u32,
            program: *const c_char,
        ),
    >,
}

unsafe extern "C" {
    pub fn ferrum_session_new(callbacks: *const ferrum_callbacks) -> *mut ferrum_session;
    pub fn ferrum_session_free(s: *mut ferrum_session);
    pub fn ferrum_session_parse_args(
        s: *mut ferrum_session,
        argc: c_int,
        argv: *const *const c_char,
    ) -> c_int;
    pub fn ferrum_session_set(
        s: *mut ferrum_session,
        name: *const c_char,
        value: *const c_char,
    ) -> bool;
    pub fn ferrum_session_set_monitors(
        s: *mut ferrum_session,
        monitors: *const ferrum_monitor,
        count: usize,
    ) -> bool;
    pub fn ferrum_session_run(s: *mut ferrum_session) -> u32;
    pub fn ferrum_session_stop(s: *mut ferrum_session);
    pub fn ferrum_error_name(code: u32) -> *const c_char;
    pub fn ferrum_error_string(code: u32) -> *const c_char;

    pub fn ferrum_input_key(s: *mut ferrum_session, down: bool, repeat: bool, scancode: u32);
    pub fn ferrum_input_unicode(s: *mut ferrum_session, down: bool, codepoint: u16);
    pub fn ferrum_input_sync_locks(
        s: *mut ferrum_session,
        scroll: bool,
        num: bool,
        caps: bool,
        kana: bool,
    );
    pub fn ferrum_input_mouse_move(s: *mut ferrum_session, x: i32, y: i32);
    pub fn ferrum_input_mouse_rel(s: *mut ferrum_session, dx: i32, dy: i32);
    pub fn ferrum_input_mouse_button(s: *mut ferrum_session, button: u32, down: bool, x: i32, y: i32);
    pub fn ferrum_input_wheel(s: *mut ferrum_session, horizontal: bool, delta: i32, x: i32, y: i32);
    pub fn ferrum_input_focus_in(s: *mut ferrum_session);

    pub fn ferrum_display_resize(s: *mut ferrum_session, width: u32, height: u32, scale: u32);
    pub fn ferrum_display_layout(
        s: *mut ferrum_session,
        monitors: *const ferrum_monitor,
        count: usize,
    );
    pub fn ferrum_refresh(s: *mut ferrum_session);
    pub fn ferrum_suppress_output(s: *mut ferrum_session, suppress: bool);

    pub fn ferrum_clip_local_offer(s: *mut ferrum_session, mimes: *const *const c_char, count: usize);
    pub fn ferrum_clip_request_remote(s: *mut ferrum_session, mime: *const c_char, request_id: u64);

    pub fn ferrum_rail_activate(s: *mut ferrum_session, window_id: u64, active: bool);
    pub fn ferrum_rail_syscommand(s: *mut ferrum_session, window_id: u64, command: u16);
    pub fn ferrum_rail_move(
        s: *mut ferrum_session,
        window_id: u64,
        left: i32,
        top: i32,
        right: i32,
        bottom: i32,
    );
    pub fn ferrum_rail_exec(
        s: *mut ferrum_session,
        program: *const c_char,
        workdir: *const c_char,
        args: *const c_char,
    );

    pub fn ferrum_malloc(size: usize) -> *mut c_void;
    pub fn ferrum_strdup(s: *const c_char) -> *mut c_char;
    pub fn ferrum_free(ptr: *mut c_void);
}
