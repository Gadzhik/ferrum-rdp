// Release builds on Windows are GUI apps: no console window.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod app;
mod clipboard;
mod dialogs;
mod editor;
mod keyring;
mod monitors;
mod remoteapp;
mod session;
mod session_view;
mod tunnel;
mod window;
mod wol;

fn main() -> gtk::glib::ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("FERRUM_LOG")
                .unwrap_or_else(|_| "ferrum=info".into()),
        )
        .init();
    app::run()
}
