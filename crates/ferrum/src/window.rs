//! Main window: connection list on the left, session tabs on the right.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use ferrum_profiles::{ConnectParams, Profile, rdpfile, redact, remmina, to_args};
use gtk::{gio, glib};
use rdp_core::keymap;
use uuid::Uuid;

use crate::app::{AppState, state};
use crate::dialogs;
use crate::editor;
use crate::keyring::{self, Kind};
use crate::session::{Overrides, PageState, SessionPage};
use crate::session_view::ScaleMode;

pub struct MainWindow {
    pub window: adw::ApplicationWindow,
    split: adw::OverlaySplitView,
    content_toolbar: adw::ToolbarView,
    pub tab_view: adw::TabView,
    content_stack: gtk::Stack,
    list: gtk::ListBox,
    search: gtk::SearchEntry,
    quick: gtk::Entry,
    toasts: adw::ToastOverlay,
    title: adw::WindowTitle,
    rows: RefCell<Vec<Option<Uuid>>>,
    hover_revealed: Cell<bool>,
}

fn icon_button(icon: &str, tooltip: &str) -> gtk::Button {
    gtk::Button::builder()
        .icon_name(icon)
        .tooltip_text(tooltip)
        .build()
}

fn session_menu() -> gio::Menu {
    let menu = gio::Menu::new();
    let keys = gio::Menu::new();
    keys.append(Some("Ctrl+Alt+Del"), Some("win.send-cad"));
    keys.append(Some("Windows Key"), Some("win.send-keys::win"));
    keys.append(Some("Alt+Tab"), Some("win.send-keys::alttab"));
    keys.append(Some("Alt+F4"), Some("win.send-keys::altf4"));
    keys.append(Some("Print Screen"), Some("win.send-keys::print"));
    keys.append(Some("Ctrl+Shift+Esc"), Some("win.send-keys::taskmgr"));
    menu.append_submenu(Some("Send Keys"), &keys);

    let view = gio::Menu::new();
    view.append(Some("Fullscreen"), Some("win.fullscreen"));
    view.append(Some("Scale to Window"), Some("win.scale::fit"));
    view.append(Some("Original Size"), Some("win.scale::native"));
    view.append(Some("View Only"), Some("win.view-only"));
    view.append(Some("Capture System Shortcuts"), Some("win.grab-keyboard"));
    menu.append_section(None, &view);

    let tools = gio::Menu::new();
    tools.append(Some("Save Screenshot…"), Some("win.screenshot"));
    tools.append(Some("Refresh Screen"), Some("win.refresh"));
    tools.append(Some("Edit Connection…"), Some("win.edit-current"));
    menu.append_section(None, &tools);

    let end = gio::Menu::new();
    end.append(Some("Reconnect"), Some("win.reconnect"));
    end.append(Some("Disconnect"), Some("win.disconnect"));
    menu.append_section(None, &end);
    menu
}

fn main_menu() -> gio::Menu {
    let menu = gio::Menu::new();
    let s = gio::Menu::new();
    s.append(Some("New Connection…"), Some("win.new-profile"));
    s.append(Some("Import .rdp File…"), Some("win.import-rdp"));
    s.append(Some("Import from Remmina"), Some("win.import-remmina"));
    menu.append_section(None, &s);
    let s = gio::Menu::new();
    s.append(Some("Keyboard Shortcuts"), Some("win.show-shortcuts"));
    s.append(Some("About Ferrum"), Some("app.about"));
    menu.append_section(None, &s);
    menu
}

fn profile_menu(id: Uuid, favorite: bool) -> gio::Menu {
    let menu = gio::Menu::new();
    let s = gio::Menu::new();
    s.append(Some("Connect"), Some(&format!("win.connect-profile::{id}")));
    s.append(Some("Edit…"), Some(&format!("win.edit-profile::{id}")));
    s.append(
        Some("Duplicate"),
        Some(&format!("win.duplicate-profile::{id}")),
    );
    s.append(
        Some(if favorite {
            "Remove from Favorites"
        } else {
            "Add to Favorites"
        }),
        Some(&format!("win.toggle-favorite::{id}")),
    );
    menu.append_section(None, &s);
    let s = gio::Menu::new();
    s.append(
        Some("Export as .rdp…"),
        Some(&format!("win.export-profile::{id}")),
    );
    s.append(
        Some("Copy FreeRDP Command"),
        Some(&format!("win.copy-command::{id}")),
    );
    menu.append_section(None, &s);
    let s = gio::Menu::new();
    s.append(Some("Delete"), Some(&format!("win.delete-profile::{id}")));
    menu.append_section(None, &s);
    menu
}

/// Parse quick-connect input: `[user@]host[:port]`.
pub fn quick_profile(input: &str) -> Option<Profile> {
    let input = input.trim().trim_start_matches("rdp://");
    if input.is_empty() {
        return None;
    }
    let (user, addr) = match input.rsplit_once('@') {
        Some((u, a)) => (Some(u), a),
        None => (None, input),
    };
    let (host, port) = rdpfile::split_host_port(addr);
    let mut p = Profile::new("", host);
    p.port = port.unwrap_or(3389);
    if let Some(u) = user {
        match u.split_once('\\') {
            Some((d, u)) => {
                p.domain = d.into();
                p.username = u.into();
            }
            None => p.username = u.into(),
        }
    }
    Some(p)
}

impl MainWindow {
    pub fn new(app: &adw::Application, detached: bool) -> Rc<Self> {
        let window = adw::ApplicationWindow::builder()
            .application(app)
            .title("Ferrum")
            .default_width(1360)
            .default_height(860)
            .width_request(360)
            .height_request(300)
            .build();

        // ---------------------------------------------------------- sidebar
        let sidebar_header = adw::HeaderBar::new();
        sidebar_header.set_title_widget(Some(&adw::WindowTitle::new("Ferrum", "")));
        let add = icon_button("list-add-symbolic", "New Connection (Ctrl+N)");
        add.set_action_name(Some("win.new-profile"));
        sidebar_header.pack_start(&add);
        let menu_button = gtk::MenuButton::builder()
            .icon_name("open-menu-symbolic")
            .menu_model(&main_menu())
            .tooltip_text("Main Menu")
            .primary(true)
            .build();
        sidebar_header.pack_end(&menu_button);

        let quick = gtk::Entry::builder()
            .placeholder_text("Quick connect: user@host:port")
            .primary_icon_name("network-server-symbolic")
            .secondary_icon_name("go-next-symbolic")
            .build();
        let search = gtk::SearchEntry::builder()
            .placeholder_text("Search connections")
            .build();
        let list = gtk::ListBox::new();
        list.set_selection_mode(gtk::SelectionMode::None);
        list.add_css_class("navigation-sidebar");
        let scroller = gtk::ScrolledWindow::builder()
            .child(&list)
            .vexpand(true)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .build();
        let sidebar_box = gtk::Box::new(gtk::Orientation::Vertical, 6);
        quick.set_margin_start(8);
        quick.set_margin_end(8);
        search.set_margin_start(8);
        search.set_margin_end(8);
        sidebar_box.append(&quick);
        sidebar_box.append(&search);
        sidebar_box.append(&scroller);
        let sidebar = adw::ToolbarView::new();
        sidebar.add_top_bar(&sidebar_header);
        sidebar.set_content(Some(&sidebar_box));

        // ---------------------------------------------------------- content
        let tab_view = adw::TabView::new();
        // Keys belong to the remote computer.
        tab_view.set_shortcuts(adw::TabViewShortcuts::NONE);
        let tab_bar = adw::TabBar::builder()
            .view(&tab_view)
            .autohide(true)
            .build();
        let content_header = adw::HeaderBar::new();
        let title = adw::WindowTitle::new("Ferrum", "");
        content_header.set_title_widget(Some(&title));
        let sidebar_toggle = gtk::ToggleButton::builder()
            .icon_name("sidebar-show-symbolic")
            .tooltip_text("Connections")
            .build();
        content_header.pack_start(&sidebar_toggle);
        let session_button = gtk::MenuButton::builder()
            .icon_name("view-more-symbolic")
            .menu_model(&session_menu())
            .tooltip_text("Session")
            .build();
        let fullscreen = icon_button("view-fullscreen-symbolic", "Fullscreen (Ctrl+Alt+Enter)");
        fullscreen.set_action_name(Some("win.fullscreen"));
        let disconnect = icon_button("network-offline-symbolic", "Disconnect");
        disconnect.set_action_name(Some("win.disconnect"));
        content_header.pack_end(&session_button);
        content_header.pack_end(&fullscreen);
        content_header.pack_end(&disconnect);

        let empty = adw::StatusPage::builder()
            .icon_name("network-server-symbolic")
            .title("No Active Sessions")
            .description("Pick a connection, or type an address into Quick connect")
            .build();
        let new_btn = gtk::Button::with_label("New Connection");
        new_btn.add_css_class("pill");
        new_btn.add_css_class("suggested-action");
        new_btn.set_halign(gtk::Align::Center);
        new_btn.set_action_name(Some("win.new-profile"));
        empty.set_child(Some(&new_btn));

        let content_stack = gtk::Stack::new();
        content_stack.add_named(&empty, Some("empty"));
        content_stack.add_named(&tab_view, Some("tabs"));
        let content_toolbar = adw::ToolbarView::new();
        content_toolbar.add_top_bar(&content_header);
        content_toolbar.add_top_bar(&tab_bar);
        content_toolbar.set_content(Some(&content_stack));

        let split = adw::OverlaySplitView::builder()
            .sidebar(&sidebar)
            .content(&content_toolbar)
            .min_sidebar_width(260.0)
            .max_sidebar_width(360.0)
            .build();
        split
            .bind_property("show-sidebar", &sidebar_toggle, "active")
            .bidirectional()
            .sync_create()
            .build();
        if detached {
            split.set_show_sidebar(false);
        }

        let toasts = adw::ToastOverlay::new();
        toasts.set_child(Some(&split));
        window.set_content(Some(&toasts));

        // Collapse the sidebar on narrow windows.
        let bp = adw::Breakpoint::new(adw::BreakpointCondition::new_length(
            adw::BreakpointConditionLengthType::MaxWidth,
            720.0,
            adw::LengthUnit::Sp,
        ));
        bp.add_setter(&split, "collapsed", Some(&true.to_value()));
        window.add_breakpoint(bp);

        let win = Rc::new(MainWindow {
            window,
            split,
            content_toolbar,
            tab_view,
            content_stack,
            list,
            search,
            quick,
            toasts,
            title,
            rows: RefCell::new(Vec::new()),
            hover_revealed: Cell::new(false),
        });
        win.setup_actions();
        win.setup_signals();
        win.setup_shortcuts(&sidebar_box);
        win.refresh_list();
        win.update_session_actions();
        win
    }

    pub fn present(&self) {
        self.window.present();
    }

    pub fn toast(&self, text: &str) {
        self.toasts.add_toast(adw::Toast::new(text));
    }

    // ------------------------------------------------------------- list

    pub fn refresh_list(&self) {
        while let Some(row) = self.list.row_at_index(0) {
            self.list.remove(&row);
        }
        let mut rows = self.rows.borrow_mut();
        rows.clear();
        let st = state();
        let store = st.store.borrow();
        let query = self.search.text().to_string();

        let header = |text: &str| {
            let label = gtk::Label::builder()
                .label(text)
                .xalign(0.0)
                .margin_top(12)
                .margin_start(6)
                .build();
            label.add_css_class("heading");
            label.add_css_class("dim-label");
            gtk::ListBoxRow::builder()
                .child(&label)
                .activatable(false)
                .selectable(false)
                .build()
        };
        let make_row = |p: &Profile| {
            let subtitle = {
                let mut s = String::new();
                if !p.username.is_empty() {
                    s.push_str(&p.username);
                    s.push('@');
                }
                s.push_str(&p.address());
                if p.remote_app.enabled {
                    s.push_str(" · RemoteApp");
                }
                s
            };
            let row = adw::ActionRow::builder()
                .title(glib::markup_escape_text(&p.display_name()))
                .subtitle(glib::markup_escape_text(&subtitle))
                .activatable(true)
                .build();
            let icon = if p.favorite {
                "starred-symbolic"
            } else if p.remote_app.enabled {
                "view-paged-symbolic"
            } else {
                "computer-symbolic"
            };
            row.add_prefix(&gtk::Image::from_icon_name(icon));
            let menu = gtk::MenuButton::builder()
                .icon_name("view-more-symbolic")
                .menu_model(&profile_menu(p.id, p.favorite))
                .valign(gtk::Align::Center)
                .build();
            menu.add_css_class("flat");
            row.add_suffix(&menu);
            row
        };

        let results = store.search(&query, None);
        if results.is_empty() {
            let label = if store.profiles().is_empty() {
                "No saved connections"
            } else {
                "No matches"
            };
            let row = header(label);
            self.list.append(&row);
            rows.push(None);
            return;
        }

        if query.trim().is_empty() {
            let favorites: Vec<&Profile> = results.iter().copied().filter(|p| p.favorite).collect();
            if !favorites.is_empty() {
                self.list.append(&header("Favorites"));
                rows.push(None);
                for p in favorites {
                    self.list.append(&make_row(p));
                    rows.push(Some(p.id));
                }
            }
            let recent = store.recent(5);
            if !recent.is_empty() && store.profiles().len() > 8 {
                self.list.append(&header("Recent"));
                rows.push(None);
                for p in recent {
                    self.list.append(&make_row(p));
                    rows.push(Some(p.id));
                }
            }
            let mut by_group: Vec<&Profile> =
                results.iter().copied().filter(|p| !p.favorite).collect();
            by_group.sort_by(|a, b| {
                a.group
                    .to_lowercase()
                    .cmp(&b.group.to_lowercase())
                    .then_with(|| {
                        a.display_name()
                            .to_lowercase()
                            .cmp(&b.display_name().to_lowercase())
                    })
            });
            let mut current: Option<String> = None;
            for p in by_group {
                if current.as_deref() != Some(p.group.as_str()) {
                    let title = if p.group.is_empty() {
                        "Connections".to_string()
                    } else {
                        p.group.replace('/', " › ")
                    };
                    self.list.append(&header(&title));
                    rows.push(None);
                    current = Some(p.group.clone());
                }
                self.list.append(&make_row(p));
                rows.push(Some(p.id));
            }
        } else {
            for p in results {
                self.list.append(&make_row(p));
                rows.push(Some(p.id));
            }
        }
    }

    // ---------------------------------------------------------- sessions

    fn current_page(&self) -> Option<Rc<SessionPage>> {
        let tab = self.tab_view.selected_page()?;
        state().page_for(&tab.child())
    }

    pub fn open_session(self: &Rc<Self>, profile: Profile, saved: bool, overrides: Overrides) {
        let st = state();
        let page = SessionPage::new(st.app.upcast_ref(), profile, saved, overrides);
        st.register_page(page.clone());
        let tab = self.tab_view.append(&page.root);
        tab.set_title(&page.title());
        tab.set_icon(Some(&gio::ThemedIcon::new(if page.is_remoteapp() {
            "view-paged-symbolic"
        } else {
            "computer-symbolic"
        })));
        tab.set_loading(true);
        self.tab_view.set_selected_page(&tab);
        self.content_stack.set_visible_child_name("tabs");
        if self.split.is_collapsed() {
            self.split.set_show_sidebar(false);
        }

        let root = page.root.clone();
        page.connect_title(glib::clone!(
            #[weak]
            root,
            move |title| {
                if let Some(tab) = find_tab(&root) {
                    tab.set_title(title);
                }
            }
        ));
        page.connect_close(glib::clone!(
            #[weak]
            root,
            move || {
                if let Some(tv) = root
                    .ancestor(adw::TabView::static_type())
                    .and_downcast::<adw::TabView>()
                    && let Some(tab) = find_tab(&root)
                {
                    tv.close_page(&tab);
                }
            }
        ));
        page.connect_fullscreen(glib::clone!(
            #[weak]
            root,
            move |value| {
                if let Some(win) = window_of(&root) {
                    let on = value.unwrap_or(!win.window.is_fullscreen());
                    win.set_fullscreen(on);
                }
            }
        ));
        page.connect_state(glib::clone!(
            #[weak]
            root,
            move |s| {
                if let Some(tab) = find_tab(&root) {
                    tab.set_loading(matches!(s, PageState::Connecting | PageState::Reconnecting));
                    tab.set_needs_attention(s == PageState::Failed);
                }
                if let Some(win) = window_of(&root) {
                    win.update_session_actions();
                }
            }
        ));
        page.connect_profile_changed(|p| {
            let st = state();
            st.store.borrow_mut().upsert(p.clone());
            st.save_store();
            st.refresh_lists();
        });
        let weak_win = Rc::downgrade(self);
        page.view().connect_pointer_motion(move |_, y| {
            if let Some(win) = weak_win.upgrade() {
                win.hover_reveal(y);
            }
        });
        page.connect();
        self.update_session_actions();
    }

    pub fn connect_profile(self: &Rc<Self>, id: Uuid) {
        let profile = state().store.borrow().get(id).cloned();
        if let Some(p) = profile {
            self.open_session(p, true, Overrides::default());
        }
    }

    pub fn set_fullscreen(&self, on: bool) {
        self.window.set_fullscreened(on);
    }

    fn on_fullscreen_changed(&self) {
        let fs = self.window.is_fullscreen();
        self.content_toolbar.set_reveal_top_bars(!fs);
        self.hover_revealed.set(false);
        if fs {
            self.split.set_show_sidebar(false);
        }
        if let Some(p) = self.current_page() {
            p.focus();
        }
    }

    /// In fullscreen, show the header bar when the pointer touches the top edge.
    fn hover_reveal(&self, y: f64) {
        if !self.window.is_fullscreen() {
            return;
        }
        if y <= 2.0 && !self.content_toolbar.reveals_top_bars() {
            self.content_toolbar.set_reveal_top_bars(true);
            self.hover_revealed.set(true);
        } else if y > 90.0 && self.hover_revealed.get() {
            self.content_toolbar.set_reveal_top_bars(false);
            self.hover_revealed.set(false);
        }
    }

    fn update_session_actions(&self) {
        let page = self.current_page();
        let connected = page
            .as_ref()
            .is_some_and(|p| p.state() == PageState::Connected);
        let has = page.is_some();
        for name in [
            "send-cad",
            "send-keys",
            "screenshot",
            "refresh",
            "scale",
            "view-only",
            "grab-keyboard",
        ] {
            if let Some(a) = self
                .window
                .lookup_action(name)
                .and_downcast::<gio::SimpleAction>()
            {
                a.set_enabled(connected);
            }
        }
        for name in ["disconnect", "reconnect", "fullscreen", "edit-current"] {
            if let Some(a) = self
                .window
                .lookup_action(name)
                .and_downcast::<gio::SimpleAction>()
            {
                a.set_enabled(has);
            }
        }
        if let Some(p) = &page {
            let view = p.view();
            if let Some(a) = self
                .window
                .lookup_action("scale")
                .and_downcast::<gio::SimpleAction>()
            {
                a.set_state(
                    &(if view.scale_mode() == ScaleMode::Native {
                        "native"
                    } else {
                        "fit"
                    })
                    .to_variant(),
                );
            }
            if let Some(a) = self
                .window
                .lookup_action("view-only")
                .and_downcast::<gio::SimpleAction>()
            {
                a.set_state(&view.view_only().to_variant());
            }
            if let Some(a) = self
                .window
                .lookup_action("grab-keyboard")
                .and_downcast::<gio::SimpleAction>()
            {
                a.set_state(&view.grab_keyboard().to_variant());
            }
            self.title.set_title(&p.title());
            self.title.set_subtitle(&p.profile().address());
            self.window
                .set_title(Some(&format!("{} — Ferrum", p.title())));
        } else {
            self.title.set_title("Ferrum");
            self.title.set_subtitle("");
            self.window.set_title(Some("Ferrum"));
        }
    }

    // ----------------------------------------------------------- profiles

    pub fn new_profile(self: &Rc<Self>, profile: Profile) {
        self.edit_profile(profile, true);
    }

    fn edit_profile(self: &Rc<Self>, profile: Profile, is_new: bool) {
        let groups = state().store.borrow().groups();
        let weak = Rc::downgrade(self);
        editor::open(&self.window, profile, is_new, groups, move |result| {
            let st = state();
            let p = result.profile;
            let id = p.id;
            if p.save_password {
                if let Some(pw) = result.password {
                    let label = format!("RDP password for {}", p.display_name());
                    glib::spawn_future_local(async move {
                        if let Err(e) = keyring::store(id, Kind::Session, &label, &pw).await {
                            tracing::warn!("keyring: {e:#}");
                        }
                    });
                }
            } else {
                glib::spawn_future_local(async move {
                    let _ = keyring::delete(id, Kind::Session).await;
                });
            }
            if p.gateway.save_password
                && let Some(pw) = result.gateway_password
            {
                let label = format!("RD Gateway password for {}", p.display_name());
                glib::spawn_future_local(async move {
                    let _ = keyring::store(id, Kind::Gateway, &label, &pw).await;
                });
            }
            st.store.borrow_mut().upsert(p);
            st.save_store();
            st.refresh_lists();
            if let Some(w) = weak.upgrade() {
                w.toast("Connection saved");
            }
        });
    }

    fn import_rdp(self: &Rc<Self>) {
        let filter = gtk::FileFilter::new();
        filter.set_name(Some("Remote Desktop files"));
        filter.add_pattern("*.rdp");
        filter.add_mime_type("application/x-rdp");
        let filters = gio::ListStore::new::<gtk::FileFilter>();
        filters.append(&filter);
        let dialog = gtk::FileDialog::builder()
            .title("Import Remote Desktop File")
            .filters(&filters)
            .build();
        let weak = Rc::downgrade(self);
        dialog.open(Some(&self.window), gio::Cancellable::NONE, move |res| {
            let (Some(win), Ok(file)) = (weak.upgrade(), res) else {
                return;
            };
            match load_rdp_file(&file) {
                Ok(p) => win.edit_profile(p, true),
                Err(e) => win.toast(&format!("Could not import: {e}")),
            }
        });
    }

    fn import_remmina(&self) {
        let Some(dir) = remmina::default_dir() else {
            return;
        };
        let imported = remmina::import_dir(&dir);
        let st = state();
        let mut added = 0;
        {
            let mut store = st.store.borrow_mut();
            for p in imported {
                let exists = store.profiles().iter().any(|e| {
                    e.host == p.host
                        && e.port == p.port
                        && e.username == p.username
                        && e.name == p.name
                });
                if !exists {
                    store.upsert(p);
                    added += 1;
                }
            }
        }
        st.save_store();
        st.refresh_lists();
        self.toast(&match added {
            0 => "No new Remmina RDP connections found".to_string(),
            1 => "Imported 1 connection from Remmina".to_string(),
            n => format!("Imported {n} connections from Remmina"),
        });
    }

    fn export_profile(&self, id: Uuid) {
        let Some(p) = state().store.borrow().get(id).cloned() else {
            return;
        };
        let dialog = gtk::FileDialog::builder()
            .title("Export Connection")
            .initial_name(format!("{}.rdp", p.display_name().replace(['/', ':'], "_")))
            .build();
        let toasts = self.toasts.clone();
        dialog.save(Some(&self.window), gio::Cancellable::NONE, move |res| {
            let Ok(file) = res else { return };
            let bytes = rdpfile::encode_utf16(&rdpfile::export(&p));
            let msg = match file.replace_contents(
                &bytes,
                None,
                false,
                gio::FileCreateFlags::REPLACE_DESTINATION,
                gio::Cancellable::NONE,
            ) {
                Ok(_) => "Exported".to_string(),
                Err(e) => format!("Export failed: {e}"),
            };
            toasts.add_toast(adw::Toast::new(&msg));
        });
    }

    // ------------------------------------------------------------ actions

    fn setup_actions(self: &Rc<Self>) {
        let win = &self.window;
        let add = |name: &str,
                   param: Option<&glib::VariantTy>,
                   f: Box<dyn Fn(&Rc<MainWindow>, Option<&glib::Variant>)>| {
            let action = gio::SimpleAction::new(name, param);
            let weak = Rc::downgrade(self);
            action.connect_activate(move |_, v| {
                if let Some(w) = weak.upgrade() {
                    f(&w, v);
                }
            });
            win.add_action(&action);
        };
        let id_of = |v: Option<&glib::Variant>| {
            v.and_then(|v| v.get::<String>())
                .and_then(|s| Uuid::parse_str(&s).ok())
        };
        let s = Some(glib::VariantTy::STRING);

        add(
            "new-profile",
            None,
            Box::new(|w, _| w.edit_profile(Profile::default(), true)),
        );
        add("import-rdp", None, Box::new(|w, _| w.import_rdp()));
        add("import-remmina", None, Box::new(|w, _| w.import_remmina()));
        add(
            "show-shortcuts",
            None,
            Box::new(|w, _| show_shortcuts(&w.window)),
        );
        add(
            "connect-profile",
            s,
            Box::new(move |w, v| {
                if let Some(id) = id_of(v) {
                    w.connect_profile(id);
                }
            }),
        );
        add(
            "edit-profile",
            s,
            Box::new(move |w, v| {
                if let Some(p) = id_of(v).and_then(|id| state().store.borrow().get(id).cloned()) {
                    w.edit_profile(p, false);
                }
            }),
        );
        add(
            "duplicate-profile",
            s,
            Box::new(move |w, v| {
                if let Some(p) = id_of(v).and_then(|id| state().store.borrow().get(id).cloned()) {
                    w.edit_profile(p.duplicate(), true);
                }
            }),
        );
        add(
            "toggle-favorite",
            s,
            Box::new(move |_, v| {
                let st = state();
                if let Some(mut p) = id_of(v).and_then(|id| st.store.borrow().get(id).cloned()) {
                    p.favorite = !p.favorite;
                    st.store.borrow_mut().upsert(p);
                    st.save_store();
                    st.refresh_lists();
                }
            }),
        );
        add(
            "export-profile",
            s,
            Box::new(move |w, v| {
                if let Some(id) = id_of(v) {
                    w.export_profile(id);
                }
            }),
        );
        add(
            "copy-command",
            s,
            Box::new(move |w, v| {
                if let Some(p) = id_of(v).and_then(|id| state().store.borrow().get(id).cloned()) {
                    let args = redact(&to_args(&p, &ConnectParams::default()));
                    let cmd = std::iter::once("xfreerdp3".to_string())
                        .chain(
                            args.iter()
                                .map(|a| glib::shell_quote(a).to_string_lossy().into_owned()),
                        )
                        .collect::<Vec<_>>()
                        .join(" ");
                    w.window.clipboard().set_text(&cmd);
                    w.toast("Command copied");
                }
            }),
        );
        add(
            "delete-profile",
            s,
            Box::new(move |w, v| {
                let Some(id) = id_of(v) else { return };
                let Some(name) = state().store.borrow().get(id).map(|p| p.display_name()) else {
                    return;
                };
                let weak = Rc::downgrade(w);
                dialogs::confirm(
                    &w.window,
                    "Delete Connection?",
                    &format!("“{name}” will be removed permanently."),
                    "_Delete",
                    true,
                    move |ok| {
                        if !ok {
                            return;
                        }
                        let st = state();
                        st.store.borrow_mut().remove(id);
                        st.save_store();
                        st.refresh_lists();
                        glib::spawn_future_local(async move {
                            let _ = keyring::delete(id, Kind::Session).await;
                            let _ = keyring::delete(id, Kind::Gateway).await;
                        });
                        if let Some(w) = weak.upgrade() {
                            w.toast("Connection deleted");
                        }
                    },
                );
            }),
        );

        // --- current session ---
        add(
            "disconnect",
            None,
            Box::new(|w, _| {
                if let Some(p) = w.current_page() {
                    p.disconnect();
                }
            }),
        );
        add(
            "reconnect",
            None,
            Box::new(|w, _| {
                if let Some(p) = w.current_page() {
                    p.connect();
                }
            }),
        );
        add(
            "fullscreen",
            None,
            Box::new(|w, _| match w.current_page() {
                Some(p) => p.toggle_fullscreen(),
                None => w.set_fullscreen(!w.window.is_fullscreen()),
            }),
        );
        add(
            "send-cad",
            None,
            Box::new(|w, _| {
                if let Some(p) = w.current_page() {
                    p.send_ctrl_alt_del();
                    p.focus();
                }
            }),
        );
        add(
            "send-keys",
            s,
            Box::new(|w, v| {
                let Some(p) = w.current_page() else { return };
                let keys: &[u32] = match v.and_then(|v| v.get::<String>()).as_deref() {
                    Some("win") => &[keymap::LEFT_WIN],
                    Some("alttab") => &[keymap::LEFT_ALT, keymap::TAB],
                    Some("altf4") => &[keymap::LEFT_ALT, 0x3E],
                    Some("print") => &[keymap::EXTENDED | 0x37],
                    Some("taskmgr") => &[keymap::LEFT_CTRL, keymap::LEFT_SHIFT, keymap::ESCAPE],
                    _ => return,
                };
                p.send_keys(keys);
                p.focus();
            }),
        );
        add(
            "screenshot",
            None,
            Box::new(|w, _| {
                if let Some(p) = w.current_page() {
                    p.screenshot(w.window.upcast_ref());
                }
            }),
        );
        add(
            "refresh",
            None,
            Box::new(|w, _| {
                if let Some(p) = w.current_page() {
                    p.refresh();
                }
            }),
        );
        add(
            "edit-current",
            None,
            Box::new(|w, _| {
                if let Some(p) = w.current_page() {
                    let profile = p.profile();
                    let saved = state().store.borrow().get(profile.id).is_some();
                    w.edit_profile(profile, !saved);
                }
            }),
        );

        // Stateful view toggles.
        let scale = gio::SimpleAction::new_stateful(
            "scale",
            Some(glib::VariantTy::STRING),
            &"fit".to_variant(),
        );
        let weak = Rc::downgrade(self);
        scale.connect_change_state(move |a, v| {
            let (Some(w), Some(v)) = (weak.upgrade(), v) else {
                return;
            };
            a.set_state(v);
            if let Some(p) = w.current_page() {
                p.set_scale_mode(if v.get::<String>().as_deref() == Some("native") {
                    ScaleMode::Native
                } else {
                    ScaleMode::Fit
                });
            }
        });
        win.add_action(&scale);
        for (name, setter) in [
            (
                "view-only",
                (|p: &SessionPage, b: bool| p.set_view_only(b)) as fn(&SessionPage, bool),
            ),
            ("grab-keyboard", |p: &SessionPage, b: bool| {
                p.set_grab_keyboard(b)
            }),
        ] {
            let action = gio::SimpleAction::new_stateful(name, None, &false.to_variant());
            let weak = Rc::downgrade(self);
            action.connect_activate(move |a, _| {
                let Some(w) = weak.upgrade() else { return };
                let value = !a.state().and_then(|s| s.get::<bool>()).unwrap_or(false);
                a.set_state(&value.to_variant());
                if let Some(p) = w.current_page() {
                    setter(&p, value);
                }
            });
            win.add_action(&action);
        }
    }

    fn setup_signals(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        self.list.connect_row_activated(move |_, row| {
            let Some(w) = weak.upgrade() else { return };
            let id = w.rows.borrow().get(row.index() as usize).copied().flatten();
            if let Some(id) = id {
                w.connect_profile(id);
            }
        });

        let weak = Rc::downgrade(self);
        self.search.connect_search_changed(move |_| {
            if let Some(w) = weak.upgrade() {
                w.refresh_list();
            }
        });

        let weak = Rc::downgrade(self);
        let quick_go = move |entry: &gtk::Entry| {
            let Some(w) = weak.upgrade() else { return };
            if let Some(p) = quick_profile(&entry.text()) {
                entry.set_text("");
                w.open_session(p, false, Overrides::default());
            }
        };
        let go = quick_go.clone();
        self.quick.connect_activate(move |e| go(e));
        self.quick.connect_icon_release(move |e, pos| {
            if pos == gtk::EntryIconPosition::Secondary {
                quick_go(e);
            }
        });

        let weak = Rc::downgrade(self);
        self.window.connect_fullscreened_notify(move |_| {
            if let Some(w) = weak.upgrade() {
                w.on_fullscreen_changed();
            }
        });

        let weak = Rc::downgrade(self);
        self.tab_view.connect_selected_page_notify(move |_| {
            if let Some(w) = weak.upgrade() {
                w.update_session_actions();
                if let Some(p) = w.current_page() {
                    p.focus();
                }
            }
        });

        let weak = Rc::downgrade(self);
        self.tab_view.connect_close_page(move |tv, tab| {
            if let Some(page) = state().page_for(&tab.child()) {
                page.shutdown();
                state().unregister_page(&page);
            }
            tv.close_page_finish(tab, true);
            if let Some(w) = weak.upgrade() {
                if tv.n_pages() <= 1 {
                    w.content_stack.set_visible_child_name("empty");
                    w.set_fullscreen(false);
                }
                w.update_session_actions();
            }
            glib::Propagation::Stop
        });

        let weak = Rc::downgrade(self);
        self.tab_view.connect_page_detached(move |tv, _, _| {
            if let Some(w) = weak.upgrade() {
                if tv.n_pages() == 0 {
                    w.content_stack.set_visible_child_name("empty");
                    // A detached-only window without tabs has no purpose.
                    if !w.split.shows_sidebar() && state().window_count() > 1 {
                        w.window.close();
                    }
                }
                w.update_session_actions();
            }
        });
        let weak = Rc::downgrade(self);
        self.tab_view.connect_page_attached(move |_, _, _| {
            if let Some(w) = weak.upgrade() {
                w.content_stack.set_visible_child_name("tabs");
                w.update_session_actions();
            }
        });

        // Dragging a tab out opens a new window.
        self.tab_view.connect_create_window(|_| {
            let st = state();
            let win = MainWindow::new(&st.app, true);
            st.register_window(win.clone());
            win.present();
            Some(win.tab_view.clone())
        });

        let weak = Rc::downgrade(self);
        self.window.connect_close_request(move |window| {
            let Some(w) = weak.upgrade() else {
                return glib::Propagation::Proceed;
            };
            let n = w.tab_view.n_pages();
            if n == 0 {
                state().unregister_window(&w);
                return glib::Propagation::Proceed;
            }
            let weak = Rc::downgrade(&w);
            let body = if n == 1 {
                "The open session will be disconnected.".to_string()
            } else {
                format!("{n} open sessions will be disconnected.")
            };
            dialogs::confirm(
                window,
                "Disconnect and Close?",
                &body,
                "_Disconnect",
                true,
                move |ok| {
                    let Some(w) = weak.upgrade() else { return };
                    if ok {
                        for i in (0..w.tab_view.n_pages()).rev() {
                            let tab = w.tab_view.nth_page(i);
                            if let Some(page) = state().page_for(&tab.child()) {
                                page.shutdown();
                                state().unregister_page(&page);
                            }
                        }
                        state().unregister_window(&w);
                        w.window.destroy();
                    }
                },
            );
            glib::Propagation::Stop
        });
    }

    fn setup_shortcuts(&self, sidebar: &gtk::Box) {
        // Scoped to the sidebar so that keys in a session reach the remote computer.
        let ctrl = gtk::ShortcutController::new();
        ctrl.set_scope(gtk::ShortcutScope::Managed);
        let add = |trigger: &str, action: &str| {
            if let (Some(t), a) = (
                gtk::ShortcutTrigger::parse_string(trigger),
                gtk::NamedAction::new(action),
            ) {
                ctrl.add_shortcut(gtk::Shortcut::new(Some(t), Some(a)));
            }
        };
        add("<Control>n", "win.new-profile");
        add("<Control>o", "win.import-rdp");
        sidebar.add_controller(ctrl);

        let focus_ctrl = gtk::ShortcutController::new();
        let search = self.search.clone();
        focus_ctrl.add_shortcut(gtk::Shortcut::new(
            gtk::ShortcutTrigger::parse_string("<Control>f"),
            Some(gtk::CallbackAction::new(move |_, _| {
                search.grab_focus();
                glib::Propagation::Stop
            })),
        ));
        let quick = self.quick.clone();
        focus_ctrl.add_shortcut(gtk::Shortcut::new(
            gtk::ShortcutTrigger::parse_string("<Control>l"),
            Some(gtk::CallbackAction::new(move |_, _| {
                quick.grab_focus();
                glib::Propagation::Stop
            })),
        ));
        sidebar.add_controller(focus_ctrl);
    }
}

fn find_tab(child: &impl IsA<gtk::Widget>) -> Option<adw::TabPage> {
    let tv = child
        .ancestor(adw::TabView::static_type())
        .and_downcast::<adw::TabView>()?;
    Some(tv.page(child))
}

fn window_of(child: &impl IsA<gtk::Widget>) -> Option<Rc<MainWindow>> {
    let window = child.root().and_downcast::<gtk::Window>()?;
    state().window_for(&window)
}

pub fn load_rdp_file(file: &gio::File) -> anyhow::Result<Profile> {
    let (bytes, _) = file.load_contents(gio::Cancellable::NONE)?;
    let name = file
        .path()
        .and_then(|p| p.file_stem().map(|s| s.to_string_lossy().into_owned()))
        .unwrap_or_default();
    Ok(rdpfile::import(&rdpfile::decode(&bytes), &name))
}

fn show_shortcuts(parent: &adw::ApplicationWindow) {
    let rows = [
        ("Ctrl+Alt+Enter", "Toggle fullscreen"),
        ("Ctrl+Alt+End", "Send Ctrl+Alt+Del"),
        ("Ctrl+Alt+Home", "Capture / release system shortcuts"),
        ("Ctrl+N", "New connection (connection list)"),
        ("Ctrl+O", "Import .rdp file (connection list)"),
        ("Ctrl+F", "Search connections"),
        ("Ctrl+L", "Quick connect"),
    ];
    let group = adw::PreferencesGroup::new();
    for (keys, what) in rows {
        let row = adw::ActionRow::builder().title(what).build();
        let l = gtk::Label::new(Some(keys));
        l.add_css_class("dim-label");
        row.add_suffix(&l);
        group.add(&row);
    }
    let page = adw::PreferencesPage::new();
    page.add(&group);
    let dialog = adw::Dialog::builder()
        .title("Keyboard Shortcuts")
        .content_width(440)
        .build();
    let tb = adw::ToolbarView::new();
    tb.add_top_bar(&adw::HeaderBar::new());
    tb.set_content(Some(&page));
    dialog.set_child(Some(&tb));
    dialog.present(Some(parent));
}

impl AppState {
    pub fn window_count(&self) -> usize {
        self.windows.borrow().len()
    }
}
