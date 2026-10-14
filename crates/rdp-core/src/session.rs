//! Safe wrapper around a shim session running on its own thread.

use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr;
use std::sync::Arc;
use std::thread::JoinHandle;

use freerdp_sys as sys;

use crate::framebuffer::Rect;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Connecting,
    Connected,
    Reconnecting,
    Disconnected,
}

/// Why FreeRDP asks for credentials (mirrors `rdp_auth_reason`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuthReason {
    Nla,
    Tls,
    Rdp,
    GatewayHttp,
    GatewayRdg,
    GatewayRpc,
    SmartcardPin,
    Rdstls,
    FidoPin,
    Other(i32),
}

impl AuthReason {
    fn from_raw(v: c_int) -> Self {
        match v {
            0 => Self::Nla,
            1 => Self::Tls,
            2 => Self::Rdp,
            3 => Self::GatewayHttp,
            4 => Self::GatewayRdg,
            5 => Self::GatewayRpc,
            6 => Self::SmartcardPin,
            7 => Self::Rdstls,
            8 => Self::FidoPin,
            v => Self::Other(v),
        }
    }

    pub fn is_gateway(&self) -> bool {
        matches!(
            self,
            Self::GatewayHttp | Self::GatewayRdg | Self::GatewayRpc
        )
    }

    pub fn is_pin(&self) -> bool {
        matches!(self, Self::SmartcardPin | Self::FidoPin)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Credentials {
    pub username: String,
    pub password: String,
    pub domain: String,
}

#[derive(Clone, Debug, Default)]
pub struct CertificateInfo {
    pub host: String,
    pub port: u16,
    pub common_name: String,
    pub subject: String,
    pub issuer: String,
    pub fingerprint: String,
    /// Previously trusted certificate, when the server certificate changed.
    pub old: Option<(String, String, String)>,
    pub flags: u32,
}

impl CertificateInfo {
    pub fn is_changed(&self) -> bool {
        self.flags & sys::VERIFY_CERT_FLAG_CHANGED != 0
    }
    pub fn is_gateway(&self) -> bool {
        self.flags & sys::VERIFY_CERT_FLAG_GATEWAY != 0
    }
    pub fn is_redirect(&self) -> bool {
        self.flags & sys::VERIFY_CERT_FLAG_REDIRECT != 0
    }
    pub fn is_name_mismatch(&self) -> bool {
        self.flags & sys::VERIFY_CERT_FLAG_MISMATCH != 0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CertVerdict {
    Reject,
    AcceptPermanently,
    AcceptOnce,
}

#[derive(Clone, Debug)]
pub enum Cursor {
    Bitmap {
        /// BGRA, premultiplied is not guaranteed (straight alpha)
        bgra: Vec<u8>,
        width: u32,
        height: u32,
        hot_x: u32,
        hot_y: u32,
    },
    Hidden,
    Default,
}

/// Monitor geometry, in local desktop coordinates.
pub type Monitor = sys::ferrum_monitor;

#[derive(Clone, Debug, Default)]
pub struct RailWindow {
    pub id: u64,
    pub new: bool,
    pub title: Option<String>,
    pub offset: Option<(i32, i32)>,
    pub size: Option<(i32, i32)>,
    pub show_state: Option<u32>,
    pub style: Option<(u32, u32)>,
    pub owner: Option<u64>,
}

pub mod rail {
    pub const SHOW_HIDE: u32 = 0;
    pub const SHOW_MINIMIZED: u32 = 2;
    pub const SHOW_MAXIMIZED: u32 = 3;
    pub const SHOW_SHOW: u32 = 5;

    pub const SC_MINIMIZE: u16 = 0xF020;
    pub const SC_MAXIMIZE: u16 = 0xF030;
    pub const SC_CLOSE: u16 = 0xF060;
    pub const SC_RESTORE: u16 = 0xF120;

    pub const WS_CHILD: u32 = 0x4000_0000;
    pub const WS_POPUP: u32 = 0x8000_0000;
    pub const WS_CAPTION: u32 = 0x00C0_0000;
    pub const WS_EX_TOOLWINDOW: u32 = 0x0000_0080;
    pub const WS_EX_TOPMOST: u32 = 0x0000_0008;
}

/// Receives session events. Methods are called from the session thread and
/// from FreeRDP channel threads; blocking methods may wait for the UI.
#[allow(unused_variables)]
pub trait Handler: Send + Sync + 'static {
    fn state(&self, state: State) {}
    /// Called when the session thread exits. `error` is 0 for a clean disconnect.
    fn finished(&self, error: u32, message: Option<String>) {}
    fn desktop_resize(&self, width: u32, height: u32) {}
    /// `data` is the whole BGRX surface; only `rects` changed.
    fn frame(&self, data: &[u8], stride: usize, width: u32, height: u32, rects: &[Rect]) {}
    fn cursor(&self, cursor: Cursor) {}
    fn cursor_position(&self, x: u32, y: u32) {}

    fn authenticate(&self, reason: AuthReason, current: Credentials) -> Option<Credentials> {
        None
    }
    fn verify_certificate(&self, info: CertificateInfo) -> CertVerdict {
        CertVerdict::Reject
    }
    fn gateway_message(&self, message: String, consent_mandatory: bool) -> bool {
        !consent_mandatory
    }
    fn choose_smartcard(&self, names: Vec<String>) -> Option<usize> {
        Some(0)
    }
    fn logon_error(&self, data: String, kind: String) {}

    fn clipboard_remote_offer(&self, mimes: Vec<String>) {}
    fn clipboard_remote_data(&self, request_id: u64, data: Option<Vec<u8>>) {}
    fn clipboard_local_get(&self, mime: &str) -> Option<Vec<u8>> {
        None
    }

    fn rail_window(&self, window: RailWindow) {}
    fn rail_window_delete(&self, id: u64) {}
    fn rail_window_icon(&self, id: u64, bgra: &[u8], width: u32, height: u32) {}
    fn rail_exec_result(&self, result: u32, raw: u32, program: String) {}
}

#[derive(Debug)]
pub enum Error {
    Create,
    Arguments(i32),
    Setting(String),
    Monitors,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Create => write!(f, "failed to create FreeRDP context"),
            Error::Arguments(c) => write!(f, "invalid connection arguments (code {c})"),
            Error::Setting(s) => write!(f, "invalid setting {s}"),
            Error::Monitors => write!(f, "invalid monitor layout"),
        }
    }
}

impl std::error::Error for Error {}

/// Connection parameters understood by FreeRDP.
#[derive(Clone, Debug, Default)]
pub struct Config {
    /// xfreerdp-style arguments without argv[0], e.g. `/v:host`, `+clipboard`.
    pub args: Vec<String>,
    /// Extra `(FreeRDP setting name, value)` pairs applied after `args`.
    pub settings: Vec<(String, String)>,
    pub monitors: Vec<Monitor>,
}

struct HandlerCell {
    handler: Arc<dyn Handler>,
}

struct Inner {
    raw: *mut sys::ferrum_session,
    cell: *mut HandlerCell,
}

// SAFETY: the shim guards all entry points that may run concurrently.
unsafe impl Send for Inner {}
unsafe impl Sync for Inner {}

impl Drop for Inner {
    fn drop(&mut self) {
        unsafe {
            sys::ferrum_session_free(self.raw);
            drop(Box::from_raw(self.cell));
        }
    }
}

/// Cheap, clonable handle for sending input to a running session.
#[derive(Clone)]
pub struct SessionHandle {
    inner: Arc<Inner>,
}

/// Owns a running session; dropping it disconnects.
pub struct Session {
    handle: SessionHandle,
    thread: Option<JoinHandle<()>>,
}

struct NoopHandler;
impl Handler for NoopHandler {}

/// Check that FreeRDP accepts the configuration, without connecting.
pub fn validate(config: &Config) -> Result<(), Error> {
    configure(config, Arc::new(NoopHandler)).map(drop)
}

fn configure(config: &Config, handler: Arc<dyn Handler>) -> Result<Arc<Inner>, Error> {
    let cell = Box::into_raw(Box::new(HandlerCell { handler }));
    let callbacks = callbacks(cell as *mut c_void);
    let raw = unsafe { sys::ferrum_session_new(&callbacks) };
    if raw.is_null() {
        unsafe { drop(Box::from_raw(cell)) };
        return Err(Error::Create);
    }
    let inner = Arc::new(Inner { raw, cell });

    let mut argv: Vec<CString> = vec![c"ferrum".to_owned()];
    argv.extend(config.args.iter().map(|a| cstring(a)));
    let ptrs: Vec<*const c_char> = argv.iter().map(|a| a.as_ptr()).collect();
    let rc = unsafe { sys::ferrum_session_parse_args(raw, ptrs.len() as c_int, ptrs.as_ptr()) };
    if rc != 0 {
        return Err(Error::Arguments(rc));
    }

    for (name, value) in &config.settings {
        let (n, v) = (cstring(name), cstring(value));
        if !unsafe { sys::ferrum_session_set(raw, n.as_ptr(), v.as_ptr()) } {
            return Err(Error::Setting(name.clone()));
        }
    }

    if !config.monitors.is_empty()
        && !unsafe {
            sys::ferrum_session_set_monitors(raw, config.monitors.as_ptr(), config.monitors.len())
        }
    {
        return Err(Error::Monitors);
    }
    Ok(inner)
}

impl Session {
    /// Configure and start connecting on a background thread.
    pub fn connect(config: &Config, handler: Arc<dyn Handler>) -> Result<Session, Error> {
        let inner = configure(config, handler)?;
        let thread_inner = inner.clone();
        let thread = std::thread::Builder::new()
            .name("rdp-session".into())
            .spawn(move || {
                let code = unsafe { sys::ferrum_session_run(thread_inner.raw) };
                let handler = unsafe { &(*thread_inner.cell).handler }.clone();
                handler.finished(code, error_message(code));
            })
            .map_err(|_| Error::Create)?;

        Ok(Session {
            handle: SessionHandle { inner },
            thread: Some(thread),
        })
    }

    pub fn handle(&self) -> SessionHandle {
        self.handle.clone()
    }

    /// Disconnect and wait for the session thread to exit.
    pub fn close(mut self) {
        self.handle.stop();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl std::ops::Deref for Session {
    type Target = SessionHandle;
    fn deref(&self) -> &SessionHandle {
        &self.handle
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        // Do not join here: drop may run on the UI thread while the session
        // thread waits on a UI dialog.
        self.handle.stop();
    }
}

/// Human readable text for a FreeRDP error code.
pub fn error_message(code: u32) -> Option<String> {
    if code == 0 {
        return None;
    }
    let text = unsafe { opt_str(sys::ferrum_error_string(code)) };
    let name = unsafe { opt_str(sys::ferrum_error_name(code)) };
    match (text, name) {
        (Some(t), Some(n)) => Some(format!("{t} ({n})")),
        (Some(t), None) => Some(t),
        (None, Some(n)) => Some(n),
        (None, None) => Some(format!("error 0x{code:08X}")),
    }
}

/// Mouse buttons in X11 numbering.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Button {
    Left,
    Middle,
    Right,
    Back,
    Forward,
}

impl Button {
    pub fn from_gdk(n: u32) -> Option<Button> {
        match n {
            1 => Some(Button::Left),
            2 => Some(Button::Middle),
            3 => Some(Button::Right),
            8 => Some(Button::Back),
            9 => Some(Button::Forward),
            _ => None,
        }
    }

    fn raw(self) -> u32 {
        match self {
            Button::Left => 1,
            Button::Middle => 2,
            Button::Right => 3,
            Button::Back => 8,
            Button::Forward => 9,
        }
    }
}

impl SessionHandle {
    fn raw(&self) -> *mut sys::ferrum_session {
        self.inner.raw
    }

    pub fn stop(&self) {
        unsafe { sys::ferrum_session_stop(self.raw()) }
    }

    /// `scancode` as returned by [`crate::keymap`].
    pub fn key(&self, scancode: u32, down: bool, repeat: bool) {
        unsafe { sys::ferrum_input_key(self.raw(), down, repeat, scancode) }
    }

    /// Type a character the keyboard layout cannot express.
    pub fn unicode(&self, ch: char, down: bool) {
        let mut buf = [0u16; 2];
        for unit in ch.encode_utf16(&mut buf) {
            unsafe { sys::ferrum_input_unicode(self.raw(), down, *unit) }
        }
    }

    pub fn sync_locks(&self, scroll: bool, num: bool, caps: bool, kana: bool) {
        unsafe { sys::ferrum_input_sync_locks(self.raw(), scroll, num, caps, kana) }
    }

    pub fn focus_in(&self) {
        unsafe { sys::ferrum_input_focus_in(self.raw()) }
    }

    pub fn mouse_move(&self, x: i32, y: i32) {
        unsafe { sys::ferrum_input_mouse_move(self.raw(), x, y) }
    }

    pub fn mouse_move_relative(&self, dx: i32, dy: i32) {
        unsafe { sys::ferrum_input_mouse_rel(self.raw(), dx, dy) }
    }

    pub fn mouse_button(&self, button: Button, down: bool, x: i32, y: i32) {
        unsafe { sys::ferrum_input_mouse_button(self.raw(), button.raw(), down, x, y) }
    }

    /// `delta` in wheel units (120 per notch); positive scrolls up / right.
    pub fn wheel(&self, horizontal: bool, delta: i32, x: i32, y: i32) {
        unsafe { sys::ferrum_input_wheel(self.raw(), horizontal, delta, x, y) }
    }

    /// Press and release a key chord, e.g. Ctrl+Alt+Del.
    pub fn send_chord(&self, scancodes: &[u32]) {
        for sc in scancodes {
            self.key(*sc, true, false);
        }
        for sc in scancodes.iter().rev() {
            self.key(*sc, false, false);
        }
    }

    /// Ask the server to change the resolution (dynamic resolution).
    pub fn resize(&self, width: u32, height: u32, scale_percent: u32) {
        unsafe { sys::ferrum_display_resize(self.raw(), width, height, scale_percent) }
    }

    pub fn layout(&self, monitors: &[Monitor]) {
        unsafe { sys::ferrum_display_layout(self.raw(), monitors.as_ptr(), monitors.len()) }
    }

    pub fn refresh(&self) {
        unsafe { sys::ferrum_refresh(self.raw()) }
    }

    /// Stop/resume graphics updates (e.g. while minimized).
    pub fn suppress_output(&self, suppress: bool) {
        unsafe { sys::ferrum_suppress_output(self.raw(), suppress) }
    }

    /// Announce the mime types currently on the local clipboard.
    pub fn clipboard_offer(&self, mimes: &[String]) {
        let owned: Vec<CString> = mimes.iter().map(|m| cstring(m)).collect();
        let ptrs: Vec<*const c_char> = owned.iter().map(|m| m.as_ptr()).collect();
        unsafe { sys::ferrum_clip_local_offer(self.raw(), ptrs.as_ptr(), ptrs.len()) }
    }

    /// Fetch remote clipboard data; the answer arrives via `clipboard_remote_data`.
    pub fn clipboard_request(&self, mime: &str, request_id: u64) {
        let m = cstring(mime);
        unsafe { sys::ferrum_clip_request_remote(self.raw(), m.as_ptr(), request_id) }
    }

    pub fn rail_activate(&self, window: u64, active: bool) {
        unsafe { sys::ferrum_rail_activate(self.raw(), window, active) }
    }

    pub fn rail_command(&self, window: u64, command: u16) {
        unsafe { sys::ferrum_rail_syscommand(self.raw(), window, command) }
    }

    pub fn rail_move(&self, window: u64, x: i32, y: i32, width: i32, height: i32) {
        unsafe { sys::ferrum_rail_move(self.raw(), window, x, y, x + width, y + height) }
    }

    pub fn rail_exec(&self, program: &str, workdir: Option<&str>, args: Option<&str>) {
        let p = cstring(program);
        let w = workdir.map(cstring);
        let a = args.map(cstring);
        unsafe {
            sys::ferrum_rail_exec(
                self.raw(),
                p.as_ptr(),
                w.as_ref().map_or(ptr::null(), |s| s.as_ptr()),
                a.as_ref().map_or(ptr::null(), |s| s.as_ptr()),
            )
        }
    }
}

// ---------------------------------------------------------------------------
// FFI glue
// ---------------------------------------------------------------------------

fn cstring(s: &str) -> CString {
    CString::new(s.replace('\0', "")).unwrap_or_default()
}

unsafe fn opt_str(p: *const c_char) -> Option<String> {
    if p.is_null() {
        None
    } else {
        Some(unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned())
    }
}

unsafe fn str_or_empty(p: *const c_char) -> String {
    unsafe { opt_str(p) }.unwrap_or_default()
}

/// Run `f` with the handler, swallowing panics (they must not cross FFI).
fn with_handler<R>(user: *mut c_void, fallback: R, f: impl FnOnce(&dyn Handler) -> R) -> R {
    if user.is_null() {
        return fallback;
    }
    let cell = unsafe { &*(user as *const HandlerCell) };
    match catch_unwind(AssertUnwindSafe(|| f(cell.handler.as_ref()))) {
        Ok(r) => r,
        Err(_) => {
            tracing::error!("panic in session callback");
            fallback
        }
    }
}

fn callbacks(user: *mut c_void) -> sys::ferrum_callbacks {
    sys::ferrum_callbacks {
        user,
        on_state: Some(cb_state),
        on_desktop_resize: Some(cb_desktop_resize),
        on_frame: Some(cb_frame),
        on_cursor: Some(cb_cursor),
        on_cursor_position: Some(cb_cursor_position),
        authenticate: Some(cb_authenticate),
        verify_certificate: Some(cb_verify_certificate),
        gateway_message: Some(cb_gateway_message),
        choose_smartcard: Some(cb_choose_smartcard),
        logon_error: Some(cb_logon_error),
        clip_remote_offer: Some(cb_clip_remote_offer),
        clip_remote_data: Some(cb_clip_remote_data),
        clip_local_get: Some(cb_clip_local_get),
        rail_window: Some(cb_rail_window),
        rail_window_delete: Some(cb_rail_window_delete),
        rail_window_icon: Some(cb_rail_window_icon),
        rail_exec_result: Some(cb_rail_exec_result),
    }
}

unsafe extern "C" fn cb_state(user: *mut c_void, state: c_int) {
    let state = match state {
        sys::FERRUM_STATE_CONNECTING => State::Connecting,
        sys::FERRUM_STATE_CONNECTED => State::Connected,
        sys::FERRUM_STATE_RECONNECTING => State::Reconnecting,
        _ => State::Disconnected,
    };
    with_handler(user, (), |h| h.state(state));
}

unsafe extern "C" fn cb_desktop_resize(user: *mut c_void, width: u32, height: u32) {
    with_handler(user, (), |h| h.desktop_resize(width, height));
}

unsafe extern "C" fn cb_frame(
    user: *mut c_void,
    data: *const u8,
    stride: u32,
    width: u32,
    height: u32,
    rects: *const i32,
    nrects: usize,
) {
    if data.is_null() || rects.is_null() {
        return;
    }
    let len = stride as usize * height as usize;
    let data = unsafe { std::slice::from_raw_parts(data, len) };
    let raw = unsafe { std::slice::from_raw_parts(rects, nrects * 4) };
    let rects: Vec<Rect> = raw
        .chunks_exact(4)
        .map(|r| Rect::new(r[0], r[1], r[2], r[3]))
        .collect();
    with_handler(user, (), |h| {
        h.frame(data, stride as usize, width, height, &rects)
    });
}

unsafe extern "C" fn cb_cursor(
    user: *mut c_void,
    kind: c_int,
    bgra: *const u8,
    width: u32,
    height: u32,
    hot_x: u32,
    hot_y: u32,
) {
    let cursor = match kind {
        sys::FERRUM_CURSOR_BITMAP if !bgra.is_null() => {
            let len = width as usize * height as usize * 4;
            Cursor::Bitmap {
                bgra: unsafe { std::slice::from_raw_parts(bgra, len) }.to_vec(),
                width,
                height,
                hot_x,
                hot_y,
            }
        }
        sys::FERRUM_CURSOR_HIDDEN => Cursor::Hidden,
        _ => Cursor::Default,
    };
    with_handler(user, (), |h| h.cursor(cursor));
}

unsafe extern "C" fn cb_cursor_position(user: *mut c_void, x: u32, y: u32) {
    with_handler(user, (), |h| h.cursor_position(x, y));
}

unsafe fn replace_cstr(slot: *mut *mut c_char, value: &str) {
    if slot.is_null() {
        return;
    }
    unsafe {
        sys::ferrum_free(*slot as *mut c_void);
        let c = cstring(value);
        *slot = sys::ferrum_strdup(c.as_ptr());
    }
}

unsafe extern "C" fn cb_authenticate(
    user: *mut c_void,
    reason: c_int,
    username: *mut *mut c_char,
    password: *mut *mut c_char,
    domain: *mut *mut c_char,
) -> bool {
    let read = |p: *mut *mut c_char| unsafe {
        if p.is_null() {
            String::new()
        } else {
            str_or_empty(*p)
        }
    };
    let current = Credentials {
        username: read(username),
        password: read(password),
        domain: read(domain),
    };
    let reason = AuthReason::from_raw(reason);
    match with_handler(user, None, |h| h.authenticate(reason, current)) {
        Some(c) => {
            unsafe {
                replace_cstr(username, &c.username);
                replace_cstr(password, &c.password);
                replace_cstr(domain, &c.domain);
            }
            true
        }
        None => false,
    }
}

unsafe extern "C" fn cb_verify_certificate(
    user: *mut c_void,
    info: *const sys::ferrum_cert_info,
) -> c_int {
    if info.is_null() {
        return sys::FERRUM_CERT_REJECT;
    }
    let i = unsafe { &*info };
    let info = unsafe {
        CertificateInfo {
            host: str_or_empty(i.host),
            port: i.port,
            common_name: str_or_empty(i.common_name),
            subject: str_or_empty(i.subject),
            issuer: str_or_empty(i.issuer),
            fingerprint: str_or_empty(i.fingerprint),
            old: opt_str(i.old_fingerprint)
                .map(|fp| (str_or_empty(i.old_subject), str_or_empty(i.old_issuer), fp)),
            flags: i.flags,
        }
    };
    match with_handler(user, CertVerdict::Reject, |h| h.verify_certificate(info)) {
        CertVerdict::Reject => sys::FERRUM_CERT_REJECT,
        CertVerdict::AcceptPermanently => sys::FERRUM_CERT_ACCEPT_PERMANENT,
        CertVerdict::AcceptOnce => sys::FERRUM_CERT_ACCEPT_ONCE,
    }
}

unsafe extern "C" fn cb_gateway_message(
    user: *mut c_void,
    _kind: u32,
    _display_mandatory: bool,
    consent_mandatory: bool,
    message: *const c_char,
) -> bool {
    let msg = unsafe { str_or_empty(message) };
    with_handler(user, !consent_mandatory, |h| {
        h.gateway_message(msg, consent_mandatory)
    })
}

unsafe extern "C" fn cb_choose_smartcard(
    user: *mut c_void,
    names: *const *const c_char,
    count: usize,
) -> c_int {
    let names: Vec<String> = (0..count)
        .map(|i| unsafe { str_or_empty(*names.add(i)) })
        .collect();
    match with_handler(user, Some(0), |h| h.choose_smartcard(names)) {
        Some(i) if i < count => i as c_int,
        _ => -1,
    }
}

unsafe extern "C" fn cb_logon_error(user: *mut c_void, data: *const c_char, kind: *const c_char) {
    let (d, k) = unsafe { (str_or_empty(data), str_or_empty(kind)) };
    with_handler(user, (), |h| h.logon_error(d, k));
}

unsafe extern "C" fn cb_clip_remote_offer(
    user: *mut c_void,
    mimes: *const *const c_char,
    count: usize,
) {
    let mimes: Vec<String> = (0..count)
        .map(|i| unsafe { str_or_empty(*mimes.add(i)) })
        .collect();
    with_handler(user, (), |h| h.clipboard_remote_offer(mimes));
}

unsafe extern "C" fn cb_clip_remote_data(user: *mut c_void, id: u64, data: *const u8, len: usize) {
    let data = if data.is_null() {
        None
    } else {
        Some(unsafe { std::slice::from_raw_parts(data, len) }.to_vec())
    };
    with_handler(user, (), |h| h.clipboard_remote_data(id, data));
}

unsafe extern "C" fn cb_clip_local_get(
    user: *mut c_void,
    mime: *const c_char,
    len: *mut usize,
) -> *mut u8 {
    let mime = unsafe { str_or_empty(mime) };
    let Some(data) = with_handler(user, None, |h| h.clipboard_local_get(&mime)) else {
        return ptr::null_mut();
    };
    unsafe {
        let buf = sys::ferrum_malloc(data.len().max(1)) as *mut u8;
        if buf.is_null() {
            return ptr::null_mut();
        }
        ptr::copy_nonoverlapping(data.as_ptr(), buf, data.len());
        if !len.is_null() {
            *len = data.len();
        }
        buf
    }
}

unsafe extern "C" fn cb_rail_window(user: *mut c_void, w: *const sys::ferrum_rail_window) {
    if w.is_null() {
        return;
    }
    let w = unsafe { &*w };
    let f = w.fields;
    let window = RailWindow {
        id: w.window_id,
        new: f & sys::FERRUM_RAIL_NEW != 0,
        title: (f & sys::FERRUM_RAIL_TITLE != 0).then(|| unsafe { str_or_empty(w.title) }),
        offset: (f & sys::FERRUM_RAIL_OFFSET != 0).then_some((w.x, w.y)),
        size: (f & sys::FERRUM_RAIL_SIZE != 0).then_some((w.width, w.height)),
        show_state: (f & sys::FERRUM_RAIL_STATE != 0).then_some(w.show_state),
        style: (f & sys::FERRUM_RAIL_STYLE != 0).then_some((w.style, w.ex_style)),
        owner: (f & sys::FERRUM_RAIL_OWNER != 0).then_some(w.owner_id),
    };
    with_handler(user, (), |h| h.rail_window(window));
}

unsafe extern "C" fn cb_rail_window_delete(user: *mut c_void, id: u64) {
    with_handler(user, (), |h| h.rail_window_delete(id));
}

unsafe extern "C" fn cb_rail_window_icon(
    user: *mut c_void,
    id: u64,
    bgra: *const u8,
    width: u32,
    height: u32,
) {
    if bgra.is_null() {
        return;
    }
    let data = unsafe { std::slice::from_raw_parts(bgra, width as usize * height as usize * 4) };
    with_handler(user, (), |h| h.rail_window_icon(id, data, width, height));
}

unsafe extern "C" fn cb_rail_exec_result(
    user: *mut c_void,
    result: u32,
    raw: u32,
    program: *const c_char,
) {
    let p = unsafe { str_or_empty(program) };
    with_handler(user, (), |h| h.rail_exec_result(result, raw, p));
}
