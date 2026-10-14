//! One RDP connection: runs the connect flow, owns the FreeRDP session and
//! turns its events into UI updates.

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{SyncSender, sync_channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use adw::prelude::*;
use ferrum_profiles::{ClipboardDirection, ConnectParams, Profile, WindowMode, to_args};
use gtk::{gdk, gio, glib};
use rdp_core::{
    AuthReason, CertVerdict, CertificateInfo, Config, Credentials, Cursor, FrameBuffer, Handler,
    RailWindow, Rect, Session, SessionHandle, State, keymap,
};

use crate::clipboard::ClipboardBridge;
use crate::dialogs;
use crate::keyring::{self, Kind};
use crate::monitors::{self, MonitorWindows};
use crate::remoteapp::RailManager;
use crate::session_view::{HostAction, ScaleMode, SessionView};
use crate::tunnel::Tunnel;
use crate::wol;

/// FreeRDP: connection cancelled by the user.
const ERRCONNECT_CONNECT_CANCELLED: u32 = 0x0002_000B;
const ERRCONNECT_LOGON_FAILURE: u32 = 0x0002_0014;
const ERRCONNECT_WRONG_PASSWORD: u32 = 0x0002_0015;
const ERRCONNECT_AUTHENTICATION_FAILED: u32 = 0x0002_0009;
const ERRCONNECT_PASSWORD_EXPIRED: u32 = 0x0002_000E;
const ERRCONNECT_PASSWORD_MUST_CHANGE: u32 = 0x0002_0018;

/// Events from the session/channel threads to the UI thread.
enum UiEvent {
    State(State),
    Finished(u32, Option<String>),
    Resize,
    Frame,
    Cursor(Cursor),
    Auth(AuthReason, Credentials, SyncSender<Option<Credentials>>),
    Certificate(CertificateInfo, SyncSender<CertVerdict>),
    GatewayMessage(String, bool, SyncSender<bool>),
    Smartcard(Vec<String>, SyncSender<Option<usize>>),
    LogonError(String, String),
    ClipOffer(Vec<String>),
    ClipData(u64, Option<Vec<u8>>),
    ClipLocalGet(String, SyncSender<Option<Vec<u8>>>),
    Rail(RailWindow),
    RailDelete(u64),
    RailIcon(u64, Vec<u8>, u32, u32),
    RailExec(u32, String),
    RailMoveSize(u64, bool, u32),
}

struct Bridge {
    tx: async_channel::Sender<UiEvent>,
    fb: Arc<Mutex<FrameBuffer>>,
    frame_pending: Arc<AtomicBool>,
}

impl Bridge {
    fn send(&self, ev: UiEvent) {
        let _ = self.tx.send_blocking(ev);
    }

    /// Send a request and block the calling (FreeRDP) thread for the answer.
    fn ask<T>(&self, make: impl FnOnce(SyncSender<T>) -> UiEvent) -> Option<T> {
        let (tx, rx) = sync_channel(1);
        self.send(make(tx));
        rx.recv().ok()
    }
}

impl Handler for Bridge {
    fn state(&self, state: State) {
        self.send(UiEvent::State(state));
    }
    fn finished(&self, error: u32, message: Option<String>) {
        self.send(UiEvent::Finished(error, message));
    }
    fn desktop_resize(&self, width: u32, height: u32) {
        self.fb.lock().unwrap().resize(width, height);
        self.send(UiEvent::Resize);
    }
    fn frame(&self, data: &[u8], stride: usize, width: u32, height: u32, rects: &[Rect]) {
        self.fb
            .lock()
            .unwrap()
            .blit(data, stride, width, height, rects);
        if !self.frame_pending.swap(true, Ordering::AcqRel) {
            self.send(UiEvent::Frame);
        }
    }
    fn cursor(&self, cursor: Cursor) {
        self.send(UiEvent::Cursor(cursor));
    }
    fn authenticate(&self, reason: AuthReason, current: Credentials) -> Option<Credentials> {
        self.ask(|tx| UiEvent::Auth(reason, current, tx)).flatten()
    }
    fn verify_certificate(&self, info: CertificateInfo) -> CertVerdict {
        self.ask(|tx| UiEvent::Certificate(info, tx))
            .unwrap_or(CertVerdict::Reject)
    }
    fn gateway_message(&self, message: String, consent_mandatory: bool) -> bool {
        self.ask(|tx| UiEvent::GatewayMessage(message, consent_mandatory, tx))
            .unwrap_or(false)
    }
    fn choose_smartcard(&self, names: Vec<String>) -> Option<usize> {
        self.ask(|tx| UiEvent::Smartcard(names, tx)).flatten()
    }
    fn logon_error(&self, data: String, kind: String) {
        self.send(UiEvent::LogonError(data, kind));
    }
    fn clipboard_remote_offer(&self, mimes: Vec<String>) {
        self.send(UiEvent::ClipOffer(mimes));
    }
    fn clipboard_remote_data(&self, request_id: u64, data: Option<Vec<u8>>) {
        self.send(UiEvent::ClipData(request_id, data));
    }
    fn clipboard_local_get(&self, mime: &str) -> Option<Vec<u8>> {
        // Bounded wait: a stuck local clipboard owner must not hang the channel.
        let (tx, rx) = sync_channel(1);
        self.send(UiEvent::ClipLocalGet(mime.to_string(), tx));
        rx.recv_timeout(Duration::from_secs(10)).ok().flatten()
    }
    fn rail_window(&self, window: RailWindow) {
        self.send(UiEvent::Rail(window));
    }
    fn rail_window_delete(&self, id: u64) {
        self.send(UiEvent::RailDelete(id));
    }
    fn rail_window_icon(&self, id: u64, bgra: &[u8], width: u32, height: u32) {
        self.send(UiEvent::RailIcon(id, bgra.to_vec(), width, height));
    }
    fn rail_exec_result(&self, result: u32, _raw: u32, program: String) {
        self.send(UiEvent::RailExec(result, program));
    }
    fn rail_local_move_size(&self, id: u64, start: bool, move_type: u32, _x: i32, _y: i32) {
        self.send(UiEvent::RailMoveSize(id, start, move_type));
    }
}

/// Runtime overrides from the command line or quick connect.
#[derive(Clone, Debug, Default)]
pub struct Overrides {
    pub password: Option<String>,
    pub extra_args: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PageState {
    Connecting,
    Connected,
    Reconnecting,
    Failed,
    Closed,
}

#[derive(Default)]
struct Callbacks {
    title: Option<Box<dyn Fn(&str)>>,
    close: Option<Box<dyn Fn()>>,
    fullscreen: Option<Box<dyn Fn(Option<bool>)>>,
    profile_changed: Option<Box<dyn Fn(&Profile)>>,
    state: Option<Box<dyn Fn(PageState)>>,
}

pub struct SessionPage {
    app: gtk::Application,
    profile: RefCell<Profile>,
    overrides: Overrides,
    /// Whether the profile is stored (passwords may be remembered).
    saved: bool,

    pub root: adw::ToastOverlay,
    stack: gtk::Stack,
    view: SessionView,
    status_page: adw::StatusPage,
    status_label: gtk::Label,
    error_page: adw::StatusPage,
    sign_in_again: gtk::Button,
    banner: adw::Banner,

    session: RefCell<Option<Session>>,
    handle: RefCell<Option<SessionHandle>>,
    fb: Arc<Mutex<FrameBuffer>>,
    frame_pending: Arc<AtomicBool>,
    texture: RefCell<Option<gdk::Texture>>,
    texture_generation: Cell<u64>,
    cursor: RefCell<Option<gdk::Cursor>>,
    clipboard: RefCell<Option<ClipboardBridge>>,
    tunnel: RefCell<Option<Tunnel>>,
    rail: RefCell<Option<Rc<RailManager>>>,
    multimon: RefCell<Option<MonitorWindows>>,
    multimon_plan: RefCell<Option<monitors::Plan>>,
    dialogs: RefCell<Vec<adw::Dialog>>,

    state: Cell<PageState>,
    epoch: Cell<u64>,
    user_disconnect: Cell<bool>,
    prompt_password: Cell<bool>,
    pending_secret: RefCell<Option<(Kind, String)>>,
    resize_serial: Cell<u64>,
    last_size_sent: Cell<(u32, u32)>,
    callbacks: RefCell<Callbacks>,
}

impl SessionPage {
    pub fn new(
        app: &gtk::Application,
        profile: Profile,
        saved: bool,
        overrides: Overrides,
    ) -> Rc<Self> {
        let view = SessionView::new();
        view.set_grab_keyboard(profile.input.grab_keyboard);

        let spinner = adw::Spinner::new();
        spinner.set_size_request(48, 48);
        let status_label = gtk::Label::new(None);
        status_label.add_css_class("dim-label");
        let cancel = gtk::Button::with_label("Cancel");
        cancel.add_css_class("pill");
        cancel.set_halign(gtk::Align::Center);
        let status_box = gtk::Box::new(gtk::Orientation::Vertical, 18);
        status_box.append(&status_label);
        status_box.append(&cancel);
        let status_page = adw::StatusPage::builder()
            .title(profile.display_name())
            .child(&status_box)
            .build();
        status_page.set_paintable(None::<&gdk::Paintable>);
        // Put the spinner where the icon goes.
        let spinner_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
        spinner_box.append(&spinner);
        spinner_box.set_halign(gtk::Align::Center);
        status_box.prepend(&spinner_box);

        let retry = gtk::Button::with_label("Reconnect");
        retry.add_css_class("pill");
        retry.add_css_class("suggested-action");
        let edit_retry = gtk::Button::with_label("Sign In Again");
        edit_retry.add_css_class("pill");
        let close = gtk::Button::with_label("Close");
        close.add_css_class("pill");
        let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        buttons.set_halign(gtk::Align::Center);
        buttons.append(&close);
        buttons.append(&edit_retry);
        buttons.append(&retry);
        let error_page = adw::StatusPage::builder()
            .icon_name("network-error-symbolic")
            .title("Connection Failed")
            .child(&buttons)
            .build();

        let ra_disconnect = gtk::Button::with_label("Disconnect");
        ra_disconnect.add_css_class("pill");
        ra_disconnect.add_css_class("destructive-action");
        ra_disconnect.set_halign(gtk::Align::Center);
        let remoteapp_page = adw::StatusPage::builder()
            .icon_name("view-paged-symbolic")
            .title(if profile.remote_app.name.is_empty() {
                profile.remote_app.program.clone()
            } else {
                profile.remote_app.name.clone()
            })
            .description("RemoteApp windows open as separate windows")
            .child(&ra_disconnect)
            .build();

        let mm_restore = gtk::Button::with_label("Show Here");
        mm_restore.add_css_class("pill");
        mm_restore.set_halign(gtk::Align::Center);
        let multimon_page = adw::StatusPage::builder()
            .icon_name("video-display-symbolic")
            .title("Fullscreen on All Monitors")
            .description("Press Ctrl+Alt+Enter to leave fullscreen")
            .child(&mm_restore)
            .build();

        let stack = gtk::Stack::new();
        stack.set_transition_type(gtk::StackTransitionType::Crossfade);
        stack.add_named(&status_page, Some("status"));
        stack.add_named(&view, Some("view"));
        stack.add_named(&error_page, Some("error"));
        stack.add_named(&remoteapp_page, Some("remoteapp"));
        stack.add_named(&multimon_page, Some("multimon"));

        let banner = adw::Banner::new("Connection lost. Reconnecting…");
        let vbox = gtk::Box::new(gtk::Orientation::Vertical, 0);
        vbox.append(&banner);
        vbox.append(&stack);
        let root = adw::ToastOverlay::new();
        root.set_child(Some(&vbox));

        let page = Rc::new(SessionPage {
            app: app.clone(),
            profile: RefCell::new(profile),
            overrides,
            saved,
            root,
            stack,
            view,
            status_page,
            status_label,
            error_page,
            sign_in_again: edit_retry.clone(),
            banner,
            session: RefCell::new(None),
            handle: RefCell::new(None),
            fb: Arc::new(Mutex::new(FrameBuffer::default())),
            frame_pending: Arc::new(AtomicBool::new(false)),
            texture: RefCell::new(None),
            texture_generation: Cell::new(0),
            cursor: RefCell::new(None),
            clipboard: RefCell::new(None),
            tunnel: RefCell::new(None),
            rail: RefCell::new(None),
            multimon: RefCell::new(None),
            multimon_plan: RefCell::new(None),
            dialogs: RefCell::new(Vec::new()),
            state: Cell::new(PageState::Connecting),
            epoch: Cell::new(0),
            user_disconnect: Cell::new(false),
            prompt_password: Cell::new(false),
            pending_secret: RefCell::new(None),
            resize_serial: Cell::new(0),
            last_size_sent: Cell::new((0, 0)),
            callbacks: RefCell::new(Callbacks::default()),
        });

        let weak = Rc::downgrade(&page);
        cancel.connect_clicked(glib::clone!(
            #[strong]
            weak,
            move |_| {
                if let Some(p) = weak.upgrade() {
                    p.disconnect();
                }
            }
        ));
        retry.connect_clicked(glib::clone!(
            #[strong]
            weak,
            move |_| {
                if let Some(p) = weak.upgrade() {
                    p.connect();
                }
            }
        ));
        edit_retry.connect_clicked(glib::clone!(
            #[strong]
            weak,
            move |_| {
                if let Some(p) = weak.upgrade() {
                    p.prompt_password.set(true);
                    p.connect();
                }
            }
        ));
        close.connect_clicked(glib::clone!(
            #[strong]
            weak,
            move |_| {
                if let Some(p) = weak.upgrade() {
                    p.request_close();
                }
            }
        ));
        ra_disconnect.connect_clicked(glib::clone!(
            #[strong]
            weak,
            move |_| {
                if let Some(p) = weak.upgrade() {
                    p.disconnect();
                }
            }
        ));
        mm_restore.connect_clicked(glib::clone!(
            #[strong]
            weak,
            move |_| {
                if let Some(p) = weak.upgrade() {
                    p.leave_multimon();
                }
            }
        ));

        page.view.connect_host_action(glib::clone!(
            #[strong]
            weak,
            move |action| {
                if let Some(p) = weak.upgrade() {
                    p.host_action(action);
                }
            }
        ));
        page.view.connect_resized(glib::clone!(
            #[strong]
            weak,
            move |_, _| {
                if let Some(p) = weak.upgrade() {
                    p.schedule_resize();
                }
            }
        ));
        // Save bandwidth while the tab is not visible.
        page.view.connect_map(glib::clone!(
            #[strong]
            weak,
            move |_| {
                if let Some(h) = weak.upgrade().and_then(|p| p.handle()) {
                    h.suppress_output(false);
                }
            }
        ));
        page.view.connect_unmap(glib::clone!(
            #[strong]
            weak,
            move |_| {
                if let Some(p) = weak.upgrade()
                    && p.multimon.borrow().is_none()
                    && p.rail.borrow().is_none()
                    && let Some(h) = p.handle()
                {
                    h.suppress_output(true);
                }
            }
        ));
        page
    }

    // ---------------------------------------------------------------- API

    pub fn profile(&self) -> Profile {
        self.profile.borrow().clone()
    }

    pub fn title(&self) -> String {
        self.profile.borrow().display_name()
    }

    pub fn state(&self) -> PageState {
        self.state.get()
    }

    pub fn view(&self) -> &SessionView {
        &self.view
    }

    pub fn handle(&self) -> Option<SessionHandle> {
        self.handle.borrow().clone()
    }

    pub fn is_remoteapp(&self) -> bool {
        self.profile.borrow().remote_app.enabled
    }

    pub fn connect_title(&self, f: impl Fn(&str) + 'static) {
        self.callbacks.borrow_mut().title = Some(Box::new(f));
    }
    pub fn connect_close(&self, f: impl Fn() + 'static) {
        self.callbacks.borrow_mut().close = Some(Box::new(f));
    }
    /// `None` toggles.
    pub fn connect_fullscreen(&self, f: impl Fn(Option<bool>) + 'static) {
        self.callbacks.borrow_mut().fullscreen = Some(Box::new(f));
    }
    pub fn connect_profile_changed(&self, f: impl Fn(&Profile) + 'static) {
        self.callbacks.borrow_mut().profile_changed = Some(Box::new(f));
    }
    pub fn connect_state(&self, f: impl Fn(PageState) + 'static) {
        self.callbacks.borrow_mut().state = Some(Box::new(f));
    }

    fn set_state(&self, state: PageState) {
        self.state.set(state);
        if let Some(cb) = self.callbacks.borrow().state.as_ref() {
            cb(state);
        }
    }

    fn request_close(&self) {
        if let Some(cb) = self.callbacks.borrow().close.as_ref() {
            cb();
        }
    }

    fn request_fullscreen(&self, value: Option<bool>) {
        if let Some(cb) = self.callbacks.borrow().fullscreen.as_ref() {
            cb(value);
        }
    }

    fn toast(&self, text: &str) {
        self.root.add_toast(adw::Toast::new(text));
    }

    fn set_status(&self, text: &str) {
        self.status_label.set_label(text);
        self.stack.set_visible_child_name("status");
    }

    pub fn send_ctrl_alt_del(&self) {
        if let Some(h) = self.handle() {
            h.send_chord(&[keymap::LEFT_CTRL, keymap::LEFT_ALT, keymap::DELETE]);
        }
    }

    pub fn send_keys(&self, scancodes: &[u32]) {
        if let Some(h) = self.handle() {
            h.send_chord(scancodes);
        }
    }

    pub fn set_scale_mode(&self, mode: ScaleMode) {
        self.view.set_scale_mode(mode);
    }

    pub fn set_view_only(&self, view_only: bool) {
        self.view.set_view_only(view_only);
    }

    pub fn set_grab_keyboard(&self, grab: bool) {
        self.view.set_grab_keyboard(grab);
    }

    pub fn refresh(&self) {
        if let Some(h) = self.handle() {
            h.refresh();
        }
    }

    pub fn focus(&self) {
        self.view.grab_focus();
    }

    /// Save the current remote screen as PNG.
    pub fn screenshot(&self, parent: &gtk::Window) {
        let Some(texture) = self.texture.borrow().clone() else {
            self.toast("Nothing to save yet");
            return;
        };
        let name = format!(
            "{}-{}.png",
            self.title().replace(['/', ' ', ':'], "_"),
            glib::DateTime::now_local()
                .map(|d| d.format("%Y%m%d-%H%M%S").unwrap_or_default().to_string())
                .unwrap_or_default()
        );
        let dialog = gtk::FileDialog::builder()
            .title("Save Screenshot")
            .initial_name(name)
            .build();
        let root = self.root.clone();
        dialog.save(Some(parent), gio::Cancellable::NONE, move |res| {
            if let Ok(file) = res
                && let Some(path) = file.path()
            {
                let msg = match texture.save_to_png(&path) {
                    Ok(()) => format!("Saved {}", path.display()),
                    Err(e) => format!("Could not save screenshot: {e}"),
                };
                root.add_toast(adw::Toast::new(&msg));
            }
        });
    }

    /// Start (or restart) connecting.
    pub fn connect(self: &Rc<Self>) {
        self.teardown();
        self.user_disconnect.set(false);
        self.epoch.set(self.epoch.get() + 1);
        self.set_state(PageState::Connecting);
        self.banner.set_revealed(false);
        self.status_page.set_title(&self.title());
        self.set_status("Preparing…");

        let page = self.clone();
        let epoch = self.epoch.get();
        glib::spawn_future_local(async move {
            if let Err(e) = page.run_connect(epoch).await
                && page.epoch.get() == epoch
                && !page.user_disconnect.get()
            {
                page.show_error(&format!("{e:#}"), false);
            }
        });
    }

    /// User-initiated disconnect; the page closes once the session ends.
    pub fn disconnect(&self) {
        self.user_disconnect.set(true);
        match self.session.borrow().as_ref() {
            Some(s) => s.stop(),
            None => self.request_close(),
        }
    }

    /// Stop everything without closing (used on teardown/retry).
    fn teardown(&self) {
        for d in self.dialogs.borrow_mut().drain(..) {
            d.force_close();
        }
        if let Some(s) = self.session.take() {
            s.stop();
        }
        self.handle.replace(None);
        self.view.set_handle(None);
        if let Some(c) = self.clipboard.take() {
            c.detach();
        }
        if let Some(r) = self.rail.take() {
            r.close_all();
        }
        self.multimon.replace(None);
        self.tunnel.replace(None);
    }

    /// Called when the tab is closed.
    pub fn shutdown(&self) {
        self.user_disconnect.set(true);
        self.epoch.set(self.epoch.get() + 1);
        self.teardown();
        self.set_state(PageState::Closed);
    }

    // ---------------------------------------------------------- connecting

    async fn run_connect(self: &Rc<Self>, epoch: u64) -> anyhow::Result<()> {
        let profile = self.profile();
        let stale = || self.epoch.get() != epoch;

        // Credentials
        let mut password = self.overrides.password.clone();
        let mut gateway_password = None;
        if password.is_none() && profile.save_password && !self.prompt_password.get() {
            self.set_status("Unlocking keyring…");
            password = keyring::lookup(profile.id, Kind::Session)
                .await
                .unwrap_or_else(|e| {
                    tracing::warn!("keyring: {e:#}");
                    None
                });
        }
        if profile.gateway.enabled()
            && !profile.gateway.same_credentials
            && profile.gateway.save_password
        {
            gateway_password = keyring::lookup(profile.id, Kind::Gateway)
                .await
                .ok()
                .flatten();
        }
        self.prompt_password.set(false);
        if stale() {
            return Ok(());
        }

        // Wake-on-LAN
        if profile.wol.enabled && !profile.wol.mac.is_empty() {
            self.set_status("Waking up the computer…");
            let wol_cfg = profile.wol.clone();
            let (host, port) = (profile.host.clone(), profile.port);
            let ssh = profile.ssh.enabled;
            let up = gio::spawn_blocking(move || -> anyhow::Result<bool> {
                wol::send(&wol_cfg.mac, &wol_cfg.broadcast, wol_cfg.port)?;
                // Behind an SSH tunnel the host is not directly reachable.
                if ssh {
                    std::thread::sleep(Duration::from_secs(wol_cfg.wait.min(30) as u64));
                    return Ok(true);
                }
                Ok(wol::wait_for_port(
                    &host,
                    port,
                    Duration::from_secs(wol_cfg.wait as u64),
                ))
            })
            .await
            .map_err(|_| anyhow::anyhow!("Wake-on-LAN failed"))??;
            if !up {
                self.toast("The computer did not answer after Wake-on-LAN; trying anyway");
            }
            if stale() {
                return Ok(());
            }
        }

        // SSH tunnel
        let mut endpoint = None;
        if profile.ssh.enabled {
            self.set_status("Opening SSH tunnel…");
            let (ssh, host, port) = (profile.ssh.clone(), profile.host.clone(), profile.port);
            let tunnel = gio::spawn_blocking(move || Tunnel::open(&ssh, &host, port))
                .await
                .map_err(|_| anyhow::anyhow!("SSH tunnel failed"))??;
            endpoint = Some(("127.0.0.1".to_string(), tunnel.local_port));
            if stale() {
                return Ok(());
            }
            self.tunnel.replace(Some(tunnel));
        }

        // Geometry: wait until the page is on screen so size and scale are real.
        for _ in 0..40 {
            if self.stack.is_mapped() && self.stack.width() > 1 {
                break;
            }
            glib::timeout_future(Duration::from_millis(50)).await;
        }
        if stale() {
            return Ok(());
        }
        let scale = self
            .root
            .native()
            .and_then(|n| n.surface())
            .map(|s| s.scale())
            .unwrap_or(1.0);
        let (mut w, mut h) = (
            (self.stack.width() as f64 * scale) as u32,
            (self.stack.height() as f64 * scale) as u32,
        );
        if w < 200 || h < 200 {
            (w, h) = (1280, 800);
        }
        let params = ConnectParams {
            password,
            gateway_password,
            endpoint,
            initial_size: Some((w, h)),
            local_scale: (scale * 100.0).round() as u32,
            keyboard_layout: None,
        };
        let mut args = to_args(&profile, &params);
        args.extend(self.overrides.extra_args.iter().cloned());

        let mut config = Config {
            args,
            ..Default::default()
        };
        let multimon = profile.display.multimon
            && profile.display.mode == WindowMode::Fullscreen
            && !profile.remote_app.enabled;
        if multimon {
            let display = self.view.display();
            let primary = self
                .view
                .native()
                .and_then(|n| n.surface())
                .and_then(|s| display.monitor_at_surface(&s));
            if let Some(plan) =
                monitors::plan(&display, &profile.display.monitors, primary.as_ref())
            {
                config.monitors = plan.monitors.clone();
                self.multimon_plan.replace(Some(plan));
            }
        }
        tracing::info!(
            "connecting with {:?}",
            ferrum_profiles::redact(&config.args)
        );

        self.set_status(&format!("Connecting to {}…", profile.address()));
        let (tx, rx) = async_channel::unbounded();
        self.frame_pending.store(false, Ordering::Release);
        let bridge = Arc::new(Bridge {
            tx,
            fb: self.fb.clone(),
            frame_pending: self.frame_pending.clone(),
        });
        let session = Session::connect(&config, bridge).map_err(|e| anyhow::anyhow!("{e}"))?;
        let handle = session.handle();
        self.handle.replace(Some(handle.clone()));
        self.view.set_handle(Some(handle));
        self.session.replace(Some(session));

        // Event pump for this connection attempt.
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            while let Ok(ev) = rx.recv().await {
                let Some(page) = weak.upgrade() else { break };
                if page.epoch.get() != epoch {
                    // Answer blocking requests of a stale session so it can exit.
                    continue;
                }
                page.handle_event(ev);
            }
        });
        Ok(())
    }

    // -------------------------------------------------------------- events

    fn handle_event(self: &Rc<Self>, ev: UiEvent) {
        match ev {
            UiEvent::State(State::Connected) => self.on_connected(),
            UiEvent::State(State::Reconnecting) => {
                self.set_state(PageState::Reconnecting);
                self.banner.set_revealed(true);
            }
            UiEvent::State(_) => {}
            UiEvent::Finished(code, msg) => self.on_finished(code, msg),
            UiEvent::Resize => {
                self.update_texture();
            }
            UiEvent::Frame => {
                self.frame_pending.store(false, Ordering::Release);
                self.update_texture();
            }
            UiEvent::Cursor(c) => self.set_cursor(c),
            UiEvent::Auth(reason, current, reply) => self.ask_credentials(reason, current, reply),
            UiEvent::Certificate(info, reply) => {
                let d = dialogs::certificate(&self.root, &info, move |v| {
                    let _ = reply.send(v);
                });
                self.track_dialog(d);
            }
            UiEvent::GatewayMessage(msg, consent, reply) => {
                let d = dialogs::gateway_message(&self.root, &msg, consent, move |ok| {
                    let _ = reply.send(ok);
                });
                self.track_dialog(d);
            }
            UiEvent::Smartcard(names, reply) => {
                let d = dialogs::choose_smartcard(&self.root, &names, move |i| {
                    let _ = reply.send(i);
                });
                self.track_dialog(d);
            }
            UiEvent::LogonError(data, kind) => {
                tracing::info!("logon info: {data} [{kind}]");
            }
            UiEvent::ClipOffer(mimes) => {
                if let Some(c) = self.clipboard.borrow().as_ref() {
                    c.remote_offer(mimes);
                }
            }
            UiEvent::ClipData(id, data) => {
                if let Some(c) = self.clipboard.borrow().as_ref() {
                    c.remote_data(id, data);
                }
            }
            UiEvent::ClipLocalGet(mime, reply) => match self.clipboard.borrow().as_ref() {
                Some(c) => c.local_get(mime, reply),
                None => {
                    let _ = reply.send(None);
                }
            },
            UiEvent::Rail(w) => {
                if let Some(r) = self.rail_manager() {
                    r.update(w);
                }
            }
            UiEvent::RailDelete(id) => {
                if let Some(r) = self.rail.borrow().as_ref() {
                    r.delete(id);
                }
            }
            UiEvent::RailIcon(id, bgra, w, h) => {
                if let Some(r) = self.rail.borrow().as_ref() {
                    r.icon(id, &bgra, w, h);
                }
            }
            UiEvent::RailExec(result, program) => {
                if result != 0 {
                    self.toast(&format!("Could not start {program} (error {result})"));
                }
            }
            UiEvent::RailMoveSize(id, start, kind) => {
                if let Some(r) = self.rail.borrow().as_ref() {
                    r.local_move_size(id, start, kind);
                }
            }
        }
    }

    fn track_dialog(&self, dialog: adw::Dialog) {
        let mut dialogs = self.dialogs.borrow_mut();
        dialogs.retain(|d| d.parent().is_some());
        dialogs.push(dialog);
    }

    fn ask_credentials(
        self: &Rc<Self>,
        reason: AuthReason,
        mut current: Credentials,
        reply: SyncSender<Option<Credentials>>,
    ) {
        let profile = self.profile();
        if current.username.is_empty() && !reason.is_gateway() {
            current.username = profile.username.clone();
            current.domain = profile.domain.clone();
        }
        let weak: Weak<Self> = Rc::downgrade(self);
        let d = dialogs::authenticate(
            &self.root,
            reason,
            current,
            &profile.display_name(),
            self.saved,
            move |answer| {
                let Some(answer) = answer else {
                    if let Some(p) = weak.upgrade() {
                        p.user_disconnect.set(true);
                    }
                    let _ = reply.send(None);
                    return;
                };
                if let Some(p) = weak.upgrade()
                    && answer.remember
                {
                    let kind = if reason.is_gateway() {
                        Kind::Gateway
                    } else {
                        Kind::Session
                    };
                    p.pending_secret
                        .replace(Some((kind, answer.credentials.password.clone())));
                    if !reason.is_gateway() {
                        let mut prof = p.profile.borrow_mut();
                        prof.username = answer.credentials.username.clone();
                        prof.domain = answer.credentials.domain.clone();
                    }
                }
                let _ = reply.send(Some(answer.credentials));
            },
        );
        self.track_dialog(d);
    }

    fn on_connected(self: &Rc<Self>) {
        self.set_state(PageState::Connected);
        self.banner.set_revealed(false);
        let profile = self.profile();
        let Some(handle) = self.handle() else { return };

        if self.clipboard.borrow().is_none() {
            let dir = profile.resources.clipboard;
            let bridge = ClipboardBridge::new(
                &self.view.display(),
                handle.clone(),
                matches!(dir, ClipboardDirection::Both | ClipboardDirection::ToLocal),
                matches!(dir, ClipboardDirection::Both | ClipboardDirection::ToRemote),
            );
            bridge.sync();
            self.clipboard.replace(Some(bridge));
        }

        if profile.remote_app.enabled {
            self.stack.set_visible_child_name("remoteapp");
            self.rail_manager();
        } else if self.multimon_plan.borrow().is_some() {
            self.enter_multimon();
        } else {
            self.stack.set_visible_child_name("view");
            self.view.grab_focus();
            if profile.display.mode == WindowMode::Fullscreen {
                self.request_fullscreen(Some(true));
            }
        }

        // Persist what was learned during this connection.
        let mut changed = false;
        if let Some((kind, secret)) = self.pending_secret.take() {
            let id = profile.id;
            let label = format!("RDP password for {}", profile.display_name());
            glib::spawn_future_local(async move {
                if let Err(e) = keyring::store(id, kind, &label, &secret).await {
                    tracing::warn!("keyring: {e:#}");
                }
            });
            let mut p = self.profile.borrow_mut();
            match kind {
                Kind::Session => p.save_password = true,
                Kind::Gateway => p.gateway.save_password = true,
            }
            changed = true;
        }
        if self.saved {
            self.profile.borrow_mut().last_connected = Some(glib::real_time() / 1_000_000);
            changed = true;
        }
        if changed {
            let p = self.profile();
            if let Some(cb) = self.callbacks.borrow().profile_changed.as_ref() {
                cb(&p);
            }
        }
    }

    fn on_finished(self: &Rc<Self>, code: u32, message: Option<String>) {
        self.session.replace(None);
        self.handle.replace(None);
        self.view.set_handle(None);
        if let Some(c) = self.clipboard.take() {
            c.detach();
        }
        if let Some(r) = self.rail.take() {
            r.close_all();
        }
        self.multimon.replace(None);
        self.tunnel.replace(None);
        for d in self.dialogs.borrow_mut().drain(..) {
            d.force_close();
        }
        self.banner.set_revealed(false);

        let user = self.user_disconnect.get() || code == ERRCONNECT_CONNECT_CANCELLED;
        if user || (code == 0 && self.state.get() == PageState::Connected) {
            self.set_state(PageState::Closed);
            self.request_fullscreen(Some(false));
            self.request_close();
            return;
        }
        let auth = matches!(
            code,
            ERRCONNECT_LOGON_FAILURE
                | ERRCONNECT_WRONG_PASSWORD
                | ERRCONNECT_AUTHENTICATION_FAILED
                | ERRCONNECT_PASSWORD_EXPIRED
                | ERRCONNECT_PASSWORD_MUST_CHANGE
        );
        let text = message.unwrap_or_else(|| "The remote computer ended the session.".into());
        self.show_error(&text, auth);
    }

    fn show_error(&self, text: &str, auth: bool) {
        self.sign_in_again.set_visible(auth);
        self.set_state(PageState::Failed);
        self.request_fullscreen(Some(false));
        self.error_page.set_title(if auth {
            "Sign-In Failed"
        } else {
            "Connection Failed"
        });
        self.error_page
            .set_description(Some(&glib::markup_escape_text(text)));
        self.stack.set_visible_child_name("error");
    }

    // ------------------------------------------------------------ graphics

    fn update_texture(&self) {
        let texture = {
            let mut fb = self.fb.lock().unwrap();
            if fb.width == 0 || fb.height == 0 {
                return;
            }
            let damage = fb.take_damage();
            let same_size = self.texture_generation.get() == fb.generation;
            let prev = self.texture.borrow().clone().filter(|_| same_size);
            if damage.is_empty() && prev.is_some() {
                return;
            }
            let bytes = glib::Bytes::from(&fb.data[..]);
            let builder = gdk::MemoryTextureBuilder::new()
                .set_bytes(Some(&bytes))
                .set_width(fb.width as i32)
                .set_height(fb.height as i32)
                .set_stride(fb.stride())
                .set_format(gdk::MemoryFormat::B8g8r8x8);
            let builder = match prev {
                Some(prev) => {
                    let rects: Vec<gtk::cairo::RectangleInt> = damage
                        .iter()
                        .map(|r| gtk::cairo::RectangleInt::new(r.x, r.y, r.w, r.h))
                        .collect();
                    let region = gtk::cairo::Region::create_rectangles(&rects);
                    builder
                        .set_update_texture(Some(&prev))
                        .set_update_region(Some(&region))
                }
                None => builder,
            };
            self.texture_generation.set(fb.generation);
            builder.build()
        };
        self.view.set_texture(Some(texture.clone()));
        if let Some(r) = self.rail.borrow().as_ref() {
            r.set_texture(Some(texture.clone()));
        }
        if let Some(m) = self.multimon.borrow().as_ref() {
            m.set_texture(Some(texture.clone()));
        }
        self.texture.replace(Some(texture));
    }

    fn set_cursor(&self, cursor: Cursor) {
        let cursor = match cursor {
            Cursor::Bitmap {
                bgra,
                width,
                height,
                hot_x,
                hot_y,
            } if width > 0 && height > 0 => {
                let bytes = glib::Bytes::from_owned(bgra);
                let tex = gdk::MemoryTexture::new(
                    width as i32,
                    height as i32,
                    gdk::MemoryFormat::B8g8r8a8,
                    &bytes,
                    width as usize * 4,
                );
                Some(gdk::Cursor::from_texture(
                    &tex,
                    hot_x as i32,
                    hot_y as i32,
                    None,
                ))
            }
            Cursor::Hidden => gdk::Cursor::from_name("none", None),
            _ => None,
        };
        self.view.set_remote_cursor(cursor.clone());
        if let Some(r) = self.rail.borrow().as_ref() {
            r.set_cursor(cursor.clone());
        }
        if let Some(m) = self.multimon.borrow().as_ref() {
            m.set_cursor(cursor.clone());
        }
        self.cursor.replace(cursor);
    }

    // ------------------------------------------------- dynamic resolution

    fn schedule_resize(self: &Rc<Self>) {
        let profile = self.profile.borrow();
        if !matches!(
            profile.display.resolution,
            ferrum_profiles::Resolution::Dynamic
        ) || profile.remote_app.enabled
            || self.multimon.borrow().is_some()
        {
            return;
        }
        drop(profile);
        let serial = self.resize_serial.get() + 1;
        self.resize_serial.set(serial);
        let weak = Rc::downgrade(self);
        glib::timeout_add_local_once(Duration::from_millis(300), move || {
            let Some(page) = weak.upgrade() else { return };
            if page.resize_serial.get() != serial || page.state.get() != PageState::Connected {
                return;
            }
            let (w, h) = page.view.device_size();
            if w < 200 || h < 200 || page.last_size_sent.get() == (w, h) {
                return;
            }
            let scale = {
                let p = page.profile.borrow();
                if p.display.scale == 0 {
                    (page.view.device_scale() * 100.0).round() as u32
                } else {
                    p.display.scale
                }
            };
            if let Some(h_) = page.handle() {
                h_.resize(w, h, scale);
                page.last_size_sent.set((w, h));
            }
        });
    }

    // ---------------------------------------------------------- multimon

    fn enter_multimon(self: &Rc<Self>) {
        let Some(plan) = self.multimon_plan.borrow().clone() else {
            return;
        };
        let Some(handle) = self.handle() else { return };
        let weak = Rc::downgrade(self);
        let windows = MonitorWindows::open(
            &self.app,
            &plan,
            handle,
            &self.title(),
            self.texture.borrow().clone(),
            self.cursor.borrow().clone(),
            move |action| {
                if let Some(p) = weak.upgrade() {
                    match action {
                        HostAction::ToggleFullscreen => p.leave_multimon(),
                        other => p.host_action(other),
                    }
                }
            },
        );
        self.multimon.replace(Some(windows));
        self.stack.set_visible_child_name("multimon");
    }

    fn leave_multimon(self: &Rc<Self>) {
        if self.multimon.take().is_some() {
            self.view.set_scale_mode(ScaleMode::Fit);
            self.stack.set_visible_child_name("view");
            self.view.grab_focus();
        }
    }

    pub fn toggle_fullscreen(self: &Rc<Self>) {
        if self.multimon.borrow().is_some() {
            self.leave_multimon();
        } else if self.multimon_plan.borrow().is_some() && self.state.get() == PageState::Connected
        {
            self.enter_multimon();
        } else {
            self.request_fullscreen(None);
        }
    }

    fn host_action(self: &Rc<Self>, action: HostAction) {
        match action {
            HostAction::ToggleFullscreen => self.toggle_fullscreen(),
            HostAction::CtrlAltDel => self.send_ctrl_alt_del(),
            HostAction::ToggleGrab => {
                let grab = !self.view.grab_keyboard();
                self.view.set_grab_keyboard(grab);
                self.toast(if grab {
                    "Keyboard shortcuts go to the remote computer"
                } else {
                    "Keyboard shortcuts released"
                });
            }
        }
    }

    // ---------------------------------------------------------- RemoteApp

    fn rail_manager(self: &Rc<Self>) -> Option<Rc<RailManager>> {
        if let Some(r) = self.rail.borrow().as_ref() {
            return Some(r.clone());
        }
        let handle = self.handle()?;
        let scale = self.view.device_scale();
        let manager = RailManager::new(&self.app, handle, scale);
        manager.set_texture(self.texture.borrow().clone());
        manager.set_cursor(self.cursor.borrow().clone());
        let weak = Rc::downgrade(self);
        manager.connect_empty(move || {
            // Last RemoteApp window closed: end the session like mstsc does.
            if let Some(p) = weak.upgrade() {
                glib::timeout_add_local_once(Duration::from_secs(2), move || {
                    if p.rail
                        .borrow()
                        .as_ref()
                        .is_some_and(|r| r.window_count() == 0)
                    {
                        p.disconnect();
                    }
                });
            }
        });
        self.rail.replace(Some(manager.clone()));
        Some(manager)
    }
}

impl Drop for SessionPage {
    fn drop(&mut self) {
        if let Some(s) = self.session.take() {
            s.stop();
        }
    }
}
