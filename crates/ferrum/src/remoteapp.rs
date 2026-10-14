//! RemoteApp (RAIL): every remote window becomes a local window showing its
//! region of the shared remote surface.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use gtk::prelude::*;
use gtk::{gdk, glib};
use rdp_core::{RailWindow, Rect, SessionHandle, rail};

use crate::session_view::{ScaleMode, SessionView};

struct AppWindow {
    window: gtk::Window,
    view: SessionView,
    rect: Cell<Rect>,
    show_state: Cell<u32>,
    popup: bool,
    /// Ignore our own size changes caused by remote geometry updates.
    resize_serial: Cell<u64>,
}

pub struct RailManager {
    handle: SessionHandle,
    app: gtk::Application,
    windows: RefCell<HashMap<u64, Rc<AppWindow>>>,
    texture: RefCell<Option<gdk::Texture>>,
    cursor: RefCell<Option<gdk::Cursor>>,
    /// Device scale of the display, remote pixels per logical pixel.
    scale: f64,
    on_empty: RefCell<Option<Box<dyn Fn()>>>,
    had_windows: Cell<bool>,
}

fn is_popup(style: u32, ex_style: u32) -> bool {
    let captioned = style & rail::WS_CAPTION == rail::WS_CAPTION;
    !captioned && (style & rail::WS_POPUP != 0 || ex_style & rail::WS_EX_TOOLWINDOW != 0)
}

impl RailManager {
    pub fn new(app: &gtk::Application, handle: SessionHandle, scale: f64) -> Rc<Self> {
        Rc::new(RailManager {
            handle,
            app: app.clone(),
            windows: RefCell::new(HashMap::new()),
            texture: RefCell::new(None),
            cursor: RefCell::new(None),
            scale: scale.max(1.0),
            on_empty: RefCell::new(None),
            had_windows: Cell::new(false),
        })
    }

    /// Called when the last remote window disappears.
    pub fn connect_empty(&self, f: impl Fn() + 'static) {
        self.on_empty.replace(Some(Box::new(f)));
    }

    pub fn window_count(&self) -> usize {
        self.windows.borrow().len()
    }

    pub fn set_texture(&self, texture: Option<gdk::Texture>) {
        for w in self.windows.borrow().values() {
            w.view.set_texture(texture.clone());
        }
        self.texture.replace(texture);
    }

    pub fn set_cursor(&self, cursor: Option<gdk::Cursor>) {
        for w in self.windows.borrow().values() {
            w.view.set_remote_cursor(cursor.clone());
        }
        self.cursor.replace(cursor);
    }

    fn logical(&self, px: i32) -> i32 {
        ((px as f64) / self.scale).round().max(1.0) as i32
    }

    pub fn update(self: &Rc<Self>, w: RailWindow) {
        let existing = self.windows.borrow().get(&w.id).cloned();
        let win = match existing {
            Some(win) => win,
            None => {
                let (style, ex_style) = w.style.unwrap_or((0, 0));
                let (x, y) = w.offset.unwrap_or((0, 0));
                let (width, height) = w.size.unwrap_or((1, 1));
                self.create(
                    w.id,
                    w.owner,
                    is_popup(style, ex_style),
                    Rect::new(x, y, width, height),
                )
            }
        };

        if let Some(title) = &w.title {
            win.window.set_title(Some(title));
        }
        let mut rect = win.rect.get();
        if let Some((x, y)) = w.offset {
            rect.x = x;
            rect.y = y;
        }
        if let Some((width, height)) = w.size {
            rect.w = width;
            rect.h = height;
        }
        if rect != win.rect.get() {
            win.rect.set(rect);
            win.view.set_viewport(Some(rect));
            if win.show_state.get() != rail::SHOW_MINIMIZED {
                win.resize_serial.set(win.resize_serial.get() + 1);
                win.window
                    .set_default_size(self.logical(rect.w), self.logical(rect.h));
            }
        }
        if let Some(state) = w.show_state {
            win.show_state.set(state);
            match state {
                rail::SHOW_HIDE => win.window.set_visible(false),
                rail::SHOW_MINIMIZED => win.window.minimize(),
                rail::SHOW_MAXIMIZED => {
                    win.window.maximize();
                    win.window.present();
                }
                _ => {
                    win.window.unmaximize();
                    win.window.present();
                }
            }
        } else if w.new {
            win.window.present();
        }
    }

    fn create(
        self: &Rc<Self>,
        id: u64,
        owner: Option<u64>,
        popup: bool,
        rect: Rect,
    ) -> Rc<AppWindow> {
        let window = gtk::Window::builder()
            .application(&self.app)
            .decorated(false)
            .default_width(self.logical(rect.w))
            .default_height(self.logical(rect.h))
            .title("RemoteApp")
            .build();
        window.add_css_class("remoteapp-window");
        if let Some(parent) =
            owner.and_then(|o| self.windows.borrow().get(&o).map(|w| w.window.clone()))
        {
            window.set_transient_for(Some(&parent));
        }

        let view = SessionView::new();
        view.set_scale_mode(ScaleMode::Native);
        view.set_handle(Some(self.handle.clone()));
        view.set_viewport(Some(rect));
        view.set_texture(self.texture.borrow().clone());
        view.set_remote_cursor(self.cursor.borrow().clone());
        view.set_grab_keyboard(false);
        window.set_child(Some(&view));

        let win = Rc::new(AppWindow {
            window: window.clone(),
            view: view.clone(),
            rect: Cell::new(rect),
            show_state: Cell::new(rail::SHOW_SHOW),
            popup,
            resize_serial: Cell::new(0),
        });

        // Local close → ask the remote window to close.
        let handle = self.handle.clone();
        window.connect_close_request(move |_| {
            handle.rail_command(id, rail::SC_CLOSE);
            glib::Propagation::Stop
        });

        // Focus follows the local window manager.
        let handle = self.handle.clone();
        window.connect_is_active_notify(move |w| {
            handle.rail_activate(id, w.is_active());
        });

        // Tiling window managers decide the size: tell the server.
        if !popup {
            let weak = Rc::downgrade(&win);
            let handle = self.handle.clone();
            let scale = self.scale;
            view.connect_resized(move |lw, lh| {
                let Some(win) = weak.upgrade() else { return };
                let serial = win.resize_serial.get();
                let weak = Rc::downgrade(&win);
                let handle = handle.clone();
                glib::timeout_add_local_once(std::time::Duration::from_millis(250), move || {
                    let Some(win) = weak.upgrade() else { return };
                    if win.resize_serial.get() != serial
                        || win.show_state.get() == rail::SHOW_MINIMIZED
                    {
                        return;
                    }
                    let r = win.rect.get();
                    let (w, h) = (
                        (lw as f64 * scale).round() as i32,
                        (lh as f64 * scale).round() as i32,
                    );
                    if (w - r.w).abs() > 2 || (h - r.h).abs() > 2 {
                        handle.rail_move(id, r.x, r.y, w, h);
                    }
                });
            });
        }

        self.windows.borrow_mut().insert(id, win.clone());
        self.had_windows.set(true);
        win
    }

    pub fn delete(&self, id: u64) {
        let removed = self.windows.borrow_mut().remove(&id);
        if let Some(win) = removed {
            win.window.destroy();
        }
        if self.windows.borrow().is_empty() && self.had_windows.get() {
            if let Some(cb) = self.on_empty.borrow().as_ref() {
                cb();
            }
        }
    }

    pub fn icon(&self, id: u64, bgra: &[u8], width: u32, height: u32) {
        // GTK4 windows take icons by name only; keep the texture for the
        // in-app window list instead.
        let _ = (id, bgra, width, height);
    }

    /// The server delegates a title-bar drag or border resize to us.
    pub fn local_move_size(&self, id: u64, start: bool, move_type: u32) {
        let Some(win) = self.windows.borrow().get(&id).cloned() else {
            return;
        };
        if !start || win.popup {
            return;
        }
        let Some(toplevel) = win.window.surface().and_downcast::<gdk::Toplevel>() else {
            return;
        };
        let Some(pointer) = WidgetExt::display(&win.window)
            .default_seat()
            .and_then(|s| s.pointer())
        else {
            return;
        };
        // The compositor takes over the pointer: release the button remotely
        // and end the server-side move loop at the unchanged position.
        let r = win.rect.get();
        self.handle
            .mouse_button(rdp_core::Button::Left, false, r.x + 1, r.y + 1);
        self.handle.rail_move(id, r.x, r.y, r.w, r.h);
        let (px, py) = win.view.last_pointer();
        let edge = match move_type {
            1 => Some(gdk::SurfaceEdge::West),
            2 => Some(gdk::SurfaceEdge::East),
            3 => Some(gdk::SurfaceEdge::North),
            4 => Some(gdk::SurfaceEdge::NorthWest),
            5 => Some(gdk::SurfaceEdge::NorthEast),
            6 => Some(gdk::SurfaceEdge::South),
            7 => Some(gdk::SurfaceEdge::SouthWest),
            8 => Some(gdk::SurfaceEdge::SouthEast),
            _ => None,
        };
        match edge {
            Some(edge) => toplevel.begin_resize(edge, Some(&pointer), 1, px, py, gdk::CURRENT_TIME),
            None if move_type == 9 => toplevel.begin_move(&pointer, 1, px, py, gdk::CURRENT_TIME),
            None => {}
        }
    }

    pub fn close_all(&self) {
        let windows: Vec<_> = self.windows.borrow_mut().drain().map(|(_, w)| w).collect();
        for w in windows {
            w.window.destroy();
        }
    }
}
