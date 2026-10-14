//! Application state shared by all windows, and command line handling.

use std::cell::{OnceCell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;

use adw::prelude::*;
use clap::Parser;
use ferrum_profiles::{Profile, Store};
use gtk::{gio, glib};

use crate::session::{Overrides, SessionPage};
use crate::window::{self, MainWindow};

pub const APP_ID: &str = "dev.ferrum.Rdp";

#[derive(Parser, Debug, Default)]
#[command(name = "ferrum", version, about = "Full-featured RDP client for Linux")]
pub struct Cli {
    /// Host to connect to ([user@]host[:port]), an .rdp file, or a saved connection name.
    pub target: Option<String>,
    /// Connect to a saved connection by name.
    #[arg(short = 'c', long = "connection")]
    pub connection: Option<String>,
    #[arg(short, long)]
    pub user: Option<String>,
    #[arg(short, long)]
    pub domain: Option<String>,
    /// Read the password from this environment variable.
    #[arg(long, value_name = "VAR")]
    pub password_env: Option<String>,
    #[arg(short, long)]
    pub fullscreen: bool,
    /// Run a RemoteApp program instead of the desktop.
    #[arg(long, value_name = "PROGRAM")]
    pub app: Option<String>,
    /// Use all monitors (with --fullscreen).
    #[arg(long)]
    pub multimon: bool,
    /// List saved connections and exit.
    #[arg(long)]
    pub list: bool,
    /// Open the editor for a new connection (prefilled with TARGET).
    #[arg(long)]
    pub new: bool,
    /// Extra FreeRDP arguments (after `--`), e.g. `-- /gfx:AVC444`.
    #[arg(last = true)]
    pub freerdp_args: Vec<String>,
}

pub struct AppState {
    pub app: adw::Application,
    pub store: RefCell<Store>,
    pub(crate) windows: RefCell<Vec<Rc<MainWindow>>>,
    pages: RefCell<Vec<Rc<SessionPage>>>,
}

thread_local! {
    static STATE: OnceCell<Rc<AppState>> = const { OnceCell::new() };
}

pub fn state() -> Rc<AppState> {
    STATE.with(|s| s.get().expect("app state").clone())
}

impl AppState {
    pub fn save_store(&self) {
        if let Err(e) = self.store.borrow().save() {
            tracing::error!("saving connections: {e:#}");
            if let Some(w) = self.windows.borrow().first() {
                w.toast(&format!("Could not save connections: {e}"));
            }
        }
    }

    pub fn refresh_lists(&self) {
        for w in self.windows.borrow().iter() {
            w.refresh_list();
        }
    }

    pub fn register_window(&self, w: Rc<MainWindow>) {
        self.windows.borrow_mut().push(w);
    }

    pub fn unregister_window(&self, w: &Rc<MainWindow>) {
        self.windows.borrow_mut().retain(|x| !Rc::ptr_eq(x, w));
    }

    pub fn window_for(&self, window: &gtk::Window) -> Option<Rc<MainWindow>> {
        self.windows
            .borrow()
            .iter()
            .find(|w| w.window.upcast_ref::<gtk::Window>() == window)
            .cloned()
    }

    pub fn register_page(&self, p: Rc<SessionPage>) {
        self.pages.borrow_mut().push(p);
    }

    pub fn unregister_page(&self, p: &Rc<SessionPage>) {
        self.pages.borrow_mut().retain(|x| !Rc::ptr_eq(x, p));
    }

    pub fn page_for(&self, child: &gtk::Widget) -> Option<Rc<SessionPage>> {
        self.pages
            .borrow()
            .iter()
            .find(|p| p.root.upcast_ref::<gtk::Widget>() == child)
            .cloned()
    }

    /// The window to open new sessions in (the one with the sidebar, if any).
    fn main_window(&self) -> Rc<MainWindow> {
        if let Some(w) = self
            .windows
            .borrow()
            .iter()
            .find(|w| w.window.is_active())
            .cloned()
        {
            return w;
        }
        if let Some(w) = self.windows.borrow().first().cloned() {
            return w;
        }
        let w = MainWindow::new(&self.app, false);
        self.register_window(w.clone());
        w
    }
}

fn css() {
    let provider = gtk::CssProvider::new();
    provider.load_from_string(
        ".session-view { background: black; }
         .remoteapp-window { background: transparent; }",
    );
    if let Some(display) = gtk::gdk::Display::default() {
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }
}

pub fn run() -> glib::ExitCode {
    let app = adw::Application::builder()
        .application_id(APP_ID)
        .flags(gio::ApplicationFlags::HANDLES_COMMAND_LINE)
        .build();

    app.connect_startup(|app| {
        css();
        let store = Store::load(Store::default_path()).unwrap_or_else(|e| {
            tracing::error!("loading connections: {e:#}");
            Store::load(std::env::temp_dir().join("ferrum-empty.toml")).expect("empty store")
        });
        let st = Rc::new(AppState {
            app: app.clone(),
            store: RefCell::new(store),
            windows: RefCell::new(Vec::new()),
            pages: RefCell::new(Vec::new()),
        });
        STATE.with(|s| {
            let _ = s.set(st);
        });

        let about = gio::SimpleAction::new("about", None);
        about.connect_activate(|_, _| {
            let dialog = adw::AboutDialog::builder()
                .application_name("Ferrum")
                .application_icon("network-server-symbolic")
                .version(env!("CARGO_PKG_VERSION"))
                .comments("Remote Desktop client for Linux, powered by FreeRDP")
                .license_type(gtk::License::Gpl30)
                .build();
            let parent = state().windows.borrow().first().map(|w| w.window.clone());
            dialog.present(parent.as_ref());
        });
        app.add_action(&about);
    });

    app.connect_activate(|_| {
        let w = state().main_window();
        w.present();
    });

    app.connect_command_line(|app, cmdline| {
        let args: Vec<String> = cmdline
            .arguments()
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        let cli = match Cli::try_parse_from(&args) {
            Ok(c) => c,
            Err(e) => {
                cmdline.printerr_literal(&e.to_string());
                return if e.use_stderr() { 2.into() } else { 0.into() };
            }
        };
        if cli.list {
            let st = state();
            for p in st.store.borrow().profiles() {
                cmdline.print_literal(&format!(
                    "{}\t{}\t{}\n",
                    p.display_name(),
                    p.address(),
                    p.group
                ));
            }
            return 0.into();
        }
        handle_cli(app, cli, cmdline.cwd());
        0.into()
    });

    app.run()
}

fn handle_cli(app: &adw::Application, cli: Cli, cwd: Option<PathBuf>) {
    let st = state();
    let window = st.main_window();
    window.present();
    let _ = app;

    let mut overrides = Overrides {
        extra_args: cli.freerdp_args.clone(),
        ..Default::default()
    };
    if let Some(var) = &cli.password_env {
        overrides.password = std::env::var(var).ok();
    }

    if cli.new {
        let mut p = cli
            .target
            .as_deref()
            .and_then(window::quick_profile)
            .unwrap_or_default();
        apply_cli(&mut p, &cli);
        window.new_profile(p);
        return;
    }

    let target = cli.connection.clone().or(cli.target.clone());
    let Some(target) = target else { return };

    // Saved connection by name?
    let saved = st.store.borrow().find_by_name(&target).cloned();
    let (mut profile, is_saved) = if let Some(p) = saved {
        (p, true)
    } else if target.to_lowercase().ends_with(".rdp") {
        let path = PathBuf::from(&target);
        let path = if path.is_relative() {
            cwd.unwrap_or_default().join(path)
        } else {
            path
        };
        match window::load_rdp_file(&gio::File::for_path(&path)) {
            Ok(p) => (p, false),
            Err(e) => {
                window.toast(&format!("Could not open {}: {e}", path.display()));
                return;
            }
        }
    } else if let Some(uri_file) = target.strip_prefix("file://") {
        match window::load_rdp_file(&gio::File::for_uri(&format!("file://{uri_file}"))) {
            Ok(p) => (p, false),
            Err(e) => {
                window.toast(&format!("Could not open {target}: {e}"));
                return;
            }
        }
    } else {
        match window::quick_profile(&target) {
            Some(p) => (p, false),
            None => return,
        }
    };

    apply_cli(&mut profile, &cli);
    window.open_session(profile, is_saved, overrides);
}

fn apply_cli(p: &mut Profile, cli: &Cli) {
    if let Some(u) = &cli.user {
        match u.split_once('\\') {
            Some((d, u)) => {
                p.domain = d.into();
                p.username = u.into();
            }
            None => p.username = u.clone(),
        }
    }
    if let Some(d) = &cli.domain {
        p.domain = d.clone();
    }
    if cli.fullscreen {
        p.display.mode = ferrum_profiles::WindowMode::Fullscreen;
    }
    if cli.multimon {
        p.display.multimon = true;
    }
    if let Some(app) = &cli.app {
        p.remote_app.enabled = true;
        p.remote_app.program = app.clone();
    }
}
