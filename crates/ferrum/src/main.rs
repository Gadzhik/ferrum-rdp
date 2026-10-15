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
#[cfg(windows)]
mod winkbd;

/// OpenSSL looks for its providers at the MSYS2 build prefix. Point it at the
/// bundled `lib/ossl-modules`, or the legacy provider (MD4, needed for NTLM)
/// is missing.
#[cfg(windows)]
fn bundle_openssl_modules() {
    if std::env::var_os("OPENSSL_MODULES").is_some() {
        return;
    }
    let dir = std::env::current_exe()
        .ok()
        .and_then(|exe| Some(exe.parent()?.parent()?.join("lib").join("ossl-modules")));
    if let Some(dir) = dir.filter(|d| d.is_dir()) {
        // SAFETY: called first thing in main, before any other thread exists.
        unsafe { std::env::set_var("OPENSSL_MODULES", dir) };
    }
}

fn main() -> gtk::glib::ExitCode {
    #[cfg(windows)]
    bundle_openssl_modules();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("FERRUM_LOG")
                .unwrap_or_else(|_| "ferrum=info".into()),
        )
        .init();
    app::run()
}
