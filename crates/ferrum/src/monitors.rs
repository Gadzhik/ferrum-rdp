//! Multi-monitor fullscreen: one fullscreen window per local monitor, each
//! showing its slice of the spanned remote desktop.

use gtk::gdk;
use gtk::prelude::*;
use rdp_core::{Monitor, Rect, SessionHandle};

use crate::session_view::{HostAction, ScaleMode, SessionView};

#[derive(Clone)]
pub struct Plan {
    /// Monitor definitions for FreeRDP (device pixels, primary first).
    pub monitors: Vec<Monitor>,
    /// Local monitor and the remote rectangle it shows.
    pub outputs: Vec<(gdk::Monitor, Rect)>,
}

pub fn all_monitors(display: &gdk::Display) -> Vec<gdk::Monitor> {
    let list = display.monitors();
    (0..list.n_items())
        .filter_map(|i| list.item(i).and_downcast::<gdk::Monitor>())
        .collect()
}

/// Human readable monitor name for settings UIs.
pub fn describe(m: &gdk::Monitor) -> String {
    let g = m.geometry();
    let name = m
        .connector()
        .map(|c| c.to_string())
        .unwrap_or_else(|| "Monitor".into());
    match m.model() {
        Some(model) => format!("{name} — {model} ({}×{})", g.width(), g.height()),
        None => format!("{name} ({}×{})", g.width(), g.height()),
    }
}

/// Build a layout from the selected connectors (empty = all monitors).
/// `primary` is the monitor the main window is on.
pub fn plan(
    display: &gdk::Display,
    selected: &[String],
    primary: Option<&gdk::Monitor>,
) -> Option<Plan> {
    let mut mons: Vec<gdk::Monitor> = all_monitors(display)
        .into_iter()
        .filter(|m| {
            selected.is_empty()
                || m.connector()
                    .is_some_and(|c| selected.iter().any(|s| s == c.as_str()))
        })
        .collect();
    if mons.is_empty() {
        return None;
    }
    // Primary first.
    if let Some(p) = primary {
        if let Some(i) = mons.iter().position(|m| m == p) {
            let m = mons.remove(i);
            mons.insert(0, m);
        }
    }
    // One scale for the whole desktop keeps the layout contiguous.
    let scale = mons[0].scale();
    let to_px = |v: i32| (v as f64 * scale).round() as i32;

    let rects: Vec<Rect> = mons
        .iter()
        .map(|m| {
            let g = m.geometry();
            Rect::new(
                to_px(g.x()),
                to_px(g.y()),
                to_px(g.width()) & !1,
                to_px(g.height()),
            )
        })
        .collect();
    let min_x = rects.iter().map(|r| r.x).min().unwrap_or(0);
    let min_y = rects.iter().map(|r| r.y).min().unwrap_or(0);

    let monitors = mons
        .iter()
        .zip(&rects)
        .enumerate()
        .map(|(i, (m, r))| Monitor {
            id: i as u32,
            x: r.x,
            y: r.y,
            width: r.w,
            height: r.h,
            primary: i == 0,
            physical_width_mm: m.width_mm().max(0) as u32,
            physical_height_mm: m.height_mm().max(0) as u32,
            orientation: 0,
            desktop_scale: (scale * 100.0).round() as u32,
            device_scale: ferrum_profiles::args::device_scale((scale * 100.0).round() as u32),
        })
        .collect();
    // Position of each monitor inside the remote framebuffer.
    let outputs = mons
        .into_iter()
        .zip(rects)
        .map(|(m, r)| (m, Rect::new(r.x - min_x, r.y - min_y, r.w, r.h)))
        .collect();
    Some(Plan { monitors, outputs })
}

pub struct MonitorWindows {
    windows: Vec<gtk::Window>,
    views: Vec<SessionView>,
}

impl MonitorWindows {
    pub fn open(
        app: &gtk::Application,
        plan: &Plan,
        handle: SessionHandle,
        title: &str,
        texture: Option<gdk::Texture>,
        cursor: Option<gdk::Cursor>,
        on_host_action: impl Fn(HostAction) + Clone + 'static,
    ) -> MonitorWindows {
        let mut windows = Vec::new();
        let mut views = Vec::new();
        for (monitor, rect) in &plan.outputs {
            let view = SessionView::new();
            view.set_scale_mode(ScaleMode::Fit);
            view.set_viewport(Some(*rect));
            view.set_handle(Some(handle.clone()));
            view.set_texture(texture.clone());
            view.set_remote_cursor(cursor.clone());
            let cb = on_host_action.clone();
            view.connect_host_action(move |a| cb(a));

            let window = gtk::Window::builder()
                .application(app)
                .title(title)
                .decorated(false)
                .child(&view)
                .build();
            window.fullscreen_on_monitor(monitor);
            window.present();
            windows.push(window);
            views.push(view);
        }
        if let Some(v) = views.first() {
            v.grab_focus();
        }
        MonitorWindows { windows, views }
    }

    pub fn set_texture(&self, texture: Option<gdk::Texture>) {
        for v in &self.views {
            v.set_texture(texture.clone());
        }
    }

    pub fn set_cursor(&self, cursor: Option<gdk::Cursor>) {
        for v in &self.views {
            v.set_remote_cursor(cursor.clone());
        }
    }

    pub fn close(&self) {
        for w in &self.windows {
            w.destroy();
        }
    }
}

impl Drop for MonitorWindows {
    fn drop(&mut self) {
        self.close();
    }
}
