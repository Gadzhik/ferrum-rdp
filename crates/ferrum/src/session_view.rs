//! Widget that displays (a region of) the remote desktop and forwards input.

use std::cell::{Cell, RefCell};
use std::collections::HashSet;

use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gdk, glib, graphene, gsk};
use rdp_core::{Button, Rect, SessionHandle, keymap};

/// How the remote image is fitted into the widget.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ScaleMode {
    /// Scale to fit, keep aspect ratio.
    #[default]
    Fit,
    /// One remote pixel per device pixel, centered.
    Native,
}

/// Local shortcuts handled by the client instead of the remote session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostAction {
    /// Ctrl+Alt+Enter
    ToggleFullscreen,
    /// Ctrl+Alt+End
    CtrlAltDel,
    /// Ctrl+Alt+Home: stop capturing system shortcuts
    ToggleGrab,
}

type HostCallback = Box<dyn Fn(HostAction)>;
type PointerCallback = Box<dyn Fn(f64, f64)>;
type ResizeCallback = Box<dyn Fn(i32, i32)>;

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct SessionView {
        pub handle: RefCell<Option<SessionHandle>>,
        pub texture: RefCell<Option<gdk::Texture>>,
        /// Region of the remote desktop shown by this view (None = all).
        pub viewport: Cell<Option<Rect>>,
        pub scale_mode: Cell<ScaleMode>,
        pub view_only: Cell<bool>,
        pub grab_keyboard: Cell<bool>,
        pub grabbed: Cell<bool>,
        pub pressed_keys: RefCell<HashSet<u32>>,
        pub pressed_buttons: RefCell<HashSet<u32>>,
        pub last_pos: Cell<(f64, f64)>,
        pub scroll_acc: Cell<(f64, f64)>,
        pub host_cb: RefCell<Option<HostCallback>>,
        pub pointer_cb: RefCell<Option<PointerCallback>>,
        pub resize_cb: RefCell<Option<ResizeCallback>>,
        pub last_alloc: Cell<(i32, i32)>,
        pub remote_cursor: RefCell<Option<gdk::Cursor>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for SessionView {
        const NAME: &'static str = "FerrumSessionView";
        type Type = super::SessionView;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for SessionView {
        fn constructed(&self) {
            self.parent_constructed();
            let obj = self.obj();
            obj.set_focusable(true);
            obj.set_can_focus(true);
            obj.set_hexpand(true);
            obj.set_vexpand(true);
            obj.set_overflow(gtk::Overflow::Hidden);
            obj.add_css_class("session-view");
            self.grab_keyboard.set(true);
            obj.setup_input();
        }
    }

    impl WidgetImpl for SessionView {
        fn size_allocate(&self, width: i32, height: i32, baseline: i32) {
            self.parent_size_allocate(width, height, baseline);
            if self.last_alloc.replace((width, height)) != (width, height) {
                if let Some(cb) = self.resize_cb.borrow().as_ref() {
                    cb(width, height);
                }
            }
        }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let obj = self.obj();
            let (w, h) = (obj.width() as f32, obj.height() as f32);
            snapshot.append_color(&gdk::RGBA::BLACK, &graphene::Rect::new(0.0, 0.0, w, h));

            let Some(texture) = self.texture.borrow().clone() else {
                return;
            };
            let Some(layout) = obj.layout() else {
                return;
            };
            let filter = if (layout.scale * obj.device_scale() - 1.0).abs() < 0.01 {
                gsk::ScalingFilter::Nearest
            } else {
                gsk::ScalingFilter::Linear
            };

            // Draw the whole texture positioned so that the viewport lands on screen.
            let tex_w = texture.width() as f32 * layout.scale as f32;
            let tex_h = texture.height() as f32 * layout.scale as f32;
            let x = layout.offset_x as f32 - layout.src.x as f32 * layout.scale as f32;
            let y = layout.offset_y as f32 - layout.src.y as f32 * layout.scale as f32;
            let clip = graphene::Rect::new(
                layout.offset_x as f32,
                layout.offset_y as f32,
                layout.src.w as f32 * layout.scale as f32,
                layout.src.h as f32 * layout.scale as f32,
            );
            snapshot.push_clip(&clip);
            snapshot.append_scaled_texture(
                &texture,
                filter,
                &graphene::Rect::new(x, y, tex_w, tex_h),
            );
            snapshot.pop();
        }
    }
}

glib::wrapper! {
    pub struct SessionView(ObjectSubclass<imp::SessionView>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for SessionView {
    fn default() -> Self {
        Self::new()
    }
}

/// Placement of the remote image inside the widget (logical pixels).
pub struct Layout {
    pub src: Rect,
    pub scale: f64,
    pub offset_x: f64,
    pub offset_y: f64,
}

impl SessionView {
    pub fn new() -> Self {
        glib::Object::new()
    }

    pub fn set_handle(&self, handle: Option<SessionHandle>) {
        self.imp().handle.replace(handle);
    }

    pub fn handle(&self) -> Option<SessionHandle> {
        self.imp().handle.borrow().clone()
    }

    pub fn set_texture(&self, texture: Option<gdk::Texture>) {
        self.imp().texture.replace(texture);
        self.queue_draw();
    }

    pub fn texture(&self) -> Option<gdk::Texture> {
        self.imp().texture.borrow().clone()
    }

    pub fn set_viewport(&self, viewport: Option<Rect>) {
        self.imp().viewport.set(viewport);
        self.queue_draw();
    }

    pub fn set_scale_mode(&self, mode: ScaleMode) {
        self.imp().scale_mode.set(mode);
        self.queue_draw();
    }

    pub fn scale_mode(&self) -> ScaleMode {
        self.imp().scale_mode.get()
    }

    pub fn set_view_only(&self, view_only: bool) {
        self.imp().view_only.set(view_only);
    }

    pub fn view_only(&self) -> bool {
        self.imp().view_only.get()
    }

    pub fn set_grab_keyboard(&self, grab: bool) {
        self.imp().grab_keyboard.set(grab);
        if self.has_focus() {
            self.apply_grab(grab);
        }
    }

    pub fn grab_keyboard(&self) -> bool {
        self.imp().grab_keyboard.get()
    }

    pub fn connect_host_action(&self, f: impl Fn(HostAction) + 'static) {
        self.imp().host_cb.replace(Some(Box::new(f)));
    }

    /// Called with widget coordinates on every pointer motion.
    pub fn connect_pointer_motion(&self, f: impl Fn(f64, f64) + 'static) {
        self.imp().pointer_cb.replace(Some(Box::new(f)));
    }

    /// Last pointer position in widget coordinates.
    pub fn last_pointer(&self) -> (f64, f64) {
        self.imp().last_pos.get()
    }

    /// Called with the new logical size whenever the widget is resized.
    pub fn connect_resized(&self, f: impl Fn(i32, i32) + 'static) {
        self.imp().resize_cb.replace(Some(Box::new(f)));
    }

    /// Surface scale (fractional on modern compositors).
    pub fn device_scale(&self) -> f64 {
        self.native()
            .and_then(|n| n.surface())
            .map(|s| s.scale())
            .unwrap_or_else(|| self.scale_factor() as f64)
    }

    /// Size of the widget in device pixels (what the remote desktop should be).
    pub fn device_size(&self) -> (u32, u32) {
        let s = self.device_scale();
        (
            (self.width() as f64 * s).round() as u32,
            (self.height() as f64 * s).round() as u32,
        )
    }

    pub fn layout(&self) -> Option<Layout> {
        let texture = self.imp().texture.borrow().clone()?;
        let full = Rect::new(0, 0, texture.width(), texture.height());
        let src = self
            .imp()
            .viewport
            .get()
            .map(|v| v.intersect(&full))
            .unwrap_or(full);
        if src.is_empty() {
            return None;
        }
        let (w, h) = (self.width() as f64, self.height() as f64);
        let scale = match self.imp().scale_mode.get() {
            ScaleMode::Fit => (w / src.w as f64).min(h / src.h as f64),
            ScaleMode::Native => 1.0 / self.device_scale(),
        };
        let offset_x = ((w - src.w as f64 * scale) / 2.0).max(0.0);
        let offset_y = ((h - src.h as f64 * scale) / 2.0).max(0.0);
        Some(Layout {
            src,
            scale,
            offset_x,
            offset_y,
        })
    }

    /// Widget coordinates → remote desktop coordinates.
    pub fn to_remote(&self, x: f64, y: f64) -> Option<(i32, i32)> {
        let l = self.layout()?;
        let rx = l.src.x as f64 + (x - l.offset_x) / l.scale;
        let ry = l.src.y as f64 + (y - l.offset_y) / l.scale;
        let rx = rx.clamp(l.src.x as f64, (l.src.right() - 1) as f64);
        let ry = ry.clamp(l.src.y as f64, (l.src.bottom() - 1) as f64);
        Some((rx as i32, ry as i32))
    }

    pub fn set_remote_cursor(&self, cursor: Option<gdk::Cursor>) {
        self.imp().remote_cursor.replace(cursor.clone());
        self.set_cursor(cursor.as_ref());
    }

    /// Release everything held down (on focus loss), so nothing gets stuck.
    pub fn release_all(&self) {
        let imp = self.imp();
        let Some(handle) = self.handle() else {
            imp.pressed_keys.borrow_mut().clear();
            imp.pressed_buttons.borrow_mut().clear();
            return;
        };
        for sc in imp.pressed_keys.borrow_mut().drain() {
            handle.key(sc, false, false);
        }
        let (x, y) = imp.last_pos.get();
        let pos = self.to_remote(x, y).unwrap_or((0, 0));
        for b in imp.pressed_buttons.borrow_mut().drain() {
            if let Some(button) = Button::from_gdk(b) {
                handle.mouse_button(button, false, pos.0, pos.1);
            }
        }
    }

    fn apply_grab(&self, grab: bool) {
        let Some(toplevel) = self
            .native()
            .and_then(|n| n.surface())
            .and_downcast::<gdk::Toplevel>()
        else {
            return;
        };
        if grab {
            toplevel.inhibit_system_shortcuts(None::<&gdk::Event>);
        } else {
            toplevel.restore_system_shortcuts();
        }
        self.imp().grabbed.set(grab);
    }

    fn sync_locks(&self) {
        let Some(handle) = self.handle() else { return };
        let Some(kbd) = self.display().default_seat().and_then(|s| s.keyboard()) else {
            return;
        };
        handle.sync_locks(
            kbd.is_scroll_locked(),
            kbd.is_num_locked(),
            kbd.is_caps_locked(),
            false,
        );
    }

    fn host_action(&self, action: HostAction) {
        if let Some(cb) = self.imp().host_cb.borrow().as_ref() {
            cb(action);
        }
    }

    fn setup_input(&self) {
        // --- focus ---
        let focus = gtk::EventControllerFocus::new();
        focus.connect_enter(glib::clone!(
            #[weak(rename_to = view)]
            self,
            move |_| {
                view.sync_locks();
                if let Some(h) = view.handle() {
                    h.focus_in();
                }
                if view.grab_keyboard() {
                    view.apply_grab(true);
                }
            }
        ));
        focus.connect_leave(glib::clone!(
            #[weak(rename_to = view)]
            self,
            move |_| {
                view.release_all();
                if view.imp().grabbed.get() {
                    view.apply_grab(false);
                }
            }
        ));
        self.add_controller(focus);

        // --- keyboard ---
        let keys = gtk::EventControllerKey::new();
        keys.connect_key_pressed(glib::clone!(
            #[weak(rename_to = view)]
            self,
            #[upgrade_or]
            glib::Propagation::Proceed,
            move |_, keyval, keycode, state| view.on_key(keyval, keycode, state, true)
        ));
        keys.connect_key_released(glib::clone!(
            #[weak(rename_to = view)]
            self,
            move |_, keyval, keycode, state| {
                view.on_key(keyval, keycode, state, false);
            }
        ));
        self.add_controller(keys);

        // --- pointer motion ---
        let motion = gtk::EventControllerMotion::new();
        motion.connect_motion(glib::clone!(
            #[weak(rename_to = view)]
            self,
            move |_, x, y| view.on_motion(x, y)
        ));
        motion.connect_enter(glib::clone!(
            #[weak(rename_to = view)]
            self,
            move |_, x, y| {
                let cursor = view.imp().remote_cursor.borrow().clone();
                view.set_cursor(cursor.as_ref());
                view.on_motion(x, y)
            }
        ));
        self.add_controller(motion);

        // --- buttons (legacy: every button independently, no gesture logic) ---
        let legacy = gtk::EventControllerLegacy::new();
        legacy.connect_event(glib::clone!(
            #[weak(rename_to = view)]
            self,
            #[upgrade_or]
            glib::Propagation::Proceed,
            move |_, event| view.on_legacy(event)
        ));
        self.add_controller(legacy);

        // --- wheel / touchpad scrolling ---
        let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::BOTH_AXES);
        scroll.connect_scroll(glib::clone!(
            #[weak(rename_to = view)]
            self,
            #[upgrade_or]
            glib::Propagation::Proceed,
            move |ctrl, dx, dy| view.on_scroll(ctrl.unit(), dx, dy)
        ));
        self.add_controller(scroll);
    }

    fn on_key(
        &self,
        keyval: gdk::Key,
        keycode: u32,
        state: gdk::ModifierType,
        down: bool,
    ) -> glib::Propagation {
        let imp = self.imp();
        let ctrl_alt = gdk::ModifierType::CONTROL_MASK | gdk::ModifierType::ALT_MASK;
        if down && state.contains(ctrl_alt) {
            let action = match keyval {
                gdk::Key::Return | gdk::Key::KP_Enter => Some(HostAction::ToggleFullscreen),
                gdk::Key::End | gdk::Key::KP_End => Some(HostAction::CtrlAltDel),
                gdk::Key::Home | gdk::Key::KP_Home => Some(HostAction::ToggleGrab),
                _ => None,
            };
            if let Some(action) = action {
                self.host_action(action);
                return glib::Propagation::Stop;
            }
        }

        if imp.view_only.get() {
            return glib::Propagation::Stop;
        }
        let Some(handle) = self.handle() else {
            return glib::Propagation::Proceed;
        };

        match keymap::xkb_to_rdp(keycode) {
            Some(sc) => {
                let repeat = if down {
                    !imp.pressed_keys.borrow_mut().insert(sc)
                } else {
                    imp.pressed_keys.borrow_mut().remove(&sc);
                    false
                };
                handle.key(sc, down, repeat);
            }
            None => {
                // Keys without a scancode (e.g. from an on-screen keyboard): send as unicode.
                if let Some(ch) = keyval.to_unicode().filter(|c| !c.is_control()) {
                    handle.unicode(ch, down);
                }
            }
        }
        glib::Propagation::Stop
    }

    fn on_motion(&self, x: f64, y: f64) {
        let imp = self.imp();
        imp.last_pos.set((x, y));
        if let Some(cb) = imp.pointer_cb.borrow().as_ref() {
            cb(x, y);
        }
        if imp.view_only.get() {
            return;
        }
        if let (Some(handle), Some((rx, ry))) = (self.handle(), self.to_remote(x, y)) {
            handle.mouse_move(rx, ry);
        }
    }

    fn on_legacy(&self, event: &gdk::Event) -> glib::Propagation {
        let down = match event.event_type() {
            gdk::EventType::ButtonPress => true,
            gdk::EventType::ButtonRelease => false,
            _ => return glib::Propagation::Proceed,
        };
        let Some(button_event) = event.downcast_ref::<gdk::ButtonEvent>() else {
            return glib::Propagation::Proceed;
        };
        let number = button_event.button();
        if down && !self.has_focus() {
            self.grab_focus();
        }
        let imp = self.imp();
        if imp.view_only.get() {
            return glib::Propagation::Stop;
        }
        let (Some(handle), Some(button)) = (self.handle(), Button::from_gdk(number)) else {
            return glib::Propagation::Stop;
        };
        let (x, y) = imp.last_pos.get();
        let Some((rx, ry)) = self.to_remote(x, y) else {
            return glib::Propagation::Stop;
        };
        if down {
            imp.pressed_buttons.borrow_mut().insert(number);
        } else if !imp.pressed_buttons.borrow_mut().remove(&number) {
            // release without press (e.g. press happened before focus): ignore
            return glib::Propagation::Stop;
        }
        handle.mouse_button(button, down, rx, ry);
        glib::Propagation::Stop
    }

    fn on_scroll(&self, unit: gdk::ScrollUnit, dx: f64, dy: f64) -> glib::Propagation {
        let imp = self.imp();
        if imp.view_only.get() {
            return glib::Propagation::Stop;
        }
        let Some(handle) = self.handle() else {
            return glib::Propagation::Proceed;
        };
        let (x, y) = imp.last_pos.get();
        let (rx, ry) = self.to_remote(x, y).unwrap_or((0, 0));

        // Convert to wheel units (120 per notch). Touchpads report pixels.
        let factor = match unit {
            gdk::ScrollUnit::Wheel => 120.0,
            _ => 120.0 / 40.0,
        };
        let (ax, ay) = imp.scroll_acc.get();
        let ax = ax + dx * factor;
        let ay = ay - dy * factor; // GTK: positive dy = down; RDP: positive = up
        let (sx, sy) = (ax.trunc() as i32, ay.trunc() as i32);
        if sy != 0 {
            handle.wheel(false, sy, rx, ry);
        }
        if sx != 0 {
            handle.wheel(true, sx, rx, ry);
        }
        imp.scroll_acc.set((ax - sx as f64, ay - sy as f64));
        glib::Propagation::Stop
    }
}
