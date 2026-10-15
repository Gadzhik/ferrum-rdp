//! Passwords in the Secret Service (GNOME Keyring, KWallet, KeePassXC...) on
//! Linux and in the Credential Manager on Windows.
#![cfg_attr(not(target_os = "linux"), allow(dead_code, unused_imports))]

use std::collections::HashMap;

#[cfg(target_os = "linux")]
use anyhow::Context;
use anyhow::Result;
use uuid::Uuid;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Session,
    Gateway,
}

impl Kind {
    fn as_str(self) -> &'static str {
        match self {
            Kind::Session => "rdp",
            Kind::Gateway => "gateway",
        }
    }
}

fn attributes(profile: Uuid, kind: Kind) -> HashMap<&'static str, String> {
    HashMap::from([
        ("application", "dev.ferrum.Rdp".to_string()),
        ("profile", profile.to_string()),
        ("kind", kind.as_str().to_string()),
    ])
}

fn as_str_map<'a>(attrs: &'a HashMap<&'static str, String>) -> HashMap<&'a str, &'a str> {
    attrs.iter().map(|(k, v)| (*k, v.as_str())).collect()
}

#[cfg(target_os = "linux")]
pub async fn store(profile: Uuid, kind: Kind, label: &str, password: &str) -> Result<()> {
    let keyring = oo7::Keyring::new().await.context("open keyring")?;
    let attrs = attributes(profile, kind);
    keyring
        .create_item(label, &as_str_map(&attrs), password.as_bytes(), true)
        .await
        .context("store password")?;
    Ok(())
}

#[cfg(target_os = "linux")]
pub async fn lookup(profile: Uuid, kind: Kind) -> Result<Option<String>> {
    let keyring = oo7::Keyring::new().await.context("open keyring")?;
    let attrs = attributes(profile, kind);
    let items = keyring
        .search_items(&as_str_map(&attrs))
        .await
        .context("search keyring")?;
    let Some(item) = items.first() else {
        return Ok(None);
    };
    let secret = item.secret().await.context("read secret")?;
    Ok(Some(String::from_utf8_lossy(&secret).into_owned()))
}

#[cfg(target_os = "linux")]
pub async fn delete(profile: Uuid, kind: Kind) -> Result<()> {
    let keyring = oo7::Keyring::new().await.context("open keyring")?;
    let attrs = attributes(profile, kind);
    keyring
        .delete(&as_str_map(&attrs))
        .await
        .context("delete password")?;
    Ok(())
}

// Windows: generic credentials in the Credential Manager, per user.

#[cfg(windows)]
mod wincred {
    use std::ffi::c_void;
    use std::os::windows::ffi::OsStrExt;

    #[repr(C)]
    #[allow(non_snake_case)]
    struct FILETIME {
        dwLowDateTime: u32,
        dwHighDateTime: u32,
    }

    #[repr(C)]
    #[allow(non_snake_case)]
    pub struct CREDENTIALW {
        Flags: u32,
        Type: u32,
        TargetName: *mut u16,
        Comment: *mut u16,
        LastWritten: FILETIME,
        CredentialBlobSize: u32,
        CredentialBlob: *mut u8,
        Persist: u32,
        AttributeCount: u32,
        Attributes: *mut c_void,
        TargetAlias: *mut u16,
        UserName: *mut u16,
    }

    #[link(name = "advapi32")]
    unsafe extern "system" {
        fn CredWriteW(credential: *const CREDENTIALW, flags: u32) -> i32;
        fn CredReadW(target: *const u16, ty: u32, flags: u32, out: *mut *mut CREDENTIALW) -> i32;
        fn CredDeleteW(target: *const u16, ty: u32, flags: u32) -> i32;
        fn CredFree(buffer: *mut c_void);
    }

    const CRED_TYPE_GENERIC: u32 = 1;
    const CRED_PERSIST_LOCAL_MACHINE: u32 = 2;
    const ERROR_NOT_FOUND: i32 = 1168;

    fn wide(s: &str) -> Vec<u16> {
        std::ffi::OsStr::new(s).encode_wide().chain(Some(0)).collect()
    }

    fn last_error() -> std::io::Error {
        std::io::Error::last_os_error()
    }

    pub fn write(target: &str, comment: &str, secret: &[u8]) -> std::io::Result<()> {
        let mut target = wide(target);
        let mut comment = wide(comment);
        let mut user = wide("ferrum");
        let cred = CREDENTIALW {
            Flags: 0,
            Type: CRED_TYPE_GENERIC,
            TargetName: target.as_mut_ptr(),
            Comment: comment.as_mut_ptr(),
            LastWritten: FILETIME {
                dwLowDateTime: 0,
                dwHighDateTime: 0,
            },
            CredentialBlobSize: secret.len() as u32,
            CredentialBlob: secret.as_ptr() as *mut u8,
            Persist: CRED_PERSIST_LOCAL_MACHINE,
            AttributeCount: 0,
            Attributes: std::ptr::null_mut(),
            TargetAlias: std::ptr::null_mut(),
            UserName: user.as_mut_ptr(),
        };
        // SAFETY: every pointer in `cred` outlives the call; CredWriteW copies them.
        if unsafe { CredWriteW(&cred, 0) } == 0 {
            return Err(last_error());
        }
        Ok(())
    }

    pub fn read(target: &str) -> std::io::Result<Option<Vec<u8>>> {
        let target = wide(target);
        let mut cred: *mut CREDENTIALW = std::ptr::null_mut();
        // SAFETY: on success `cred` points to a buffer we release with CredFree.
        if unsafe { CredReadW(target.as_ptr(), CRED_TYPE_GENERIC, 0, &mut cred) } == 0 {
            let err = last_error();
            return if err.raw_os_error() == Some(ERROR_NOT_FOUND) {
                Ok(None)
            } else {
                Err(err)
            };
        }
        // SAFETY: CredReadW succeeded, so `cred` and its blob are valid until CredFree.
        let secret = unsafe {
            let c = &*cred;
            let blob = if c.CredentialBlob.is_null() {
                Vec::new()
            } else {
                std::slice::from_raw_parts(c.CredentialBlob, c.CredentialBlobSize as usize).to_vec()
            };
            CredFree(cred.cast());
            blob
        };
        Ok(Some(secret))
    }

    pub fn delete(target: &str) -> std::io::Result<()> {
        let target = wide(target);
        // SAFETY: plain call with a NUL-terminated string.
        if unsafe { CredDeleteW(target.as_ptr(), CRED_TYPE_GENERIC, 0) } == 0 {
            let err = last_error();
            if err.raw_os_error() != Some(ERROR_NOT_FOUND) {
                return Err(err);
            }
        }
        Ok(())
    }
}

#[cfg(windows)]
fn target_name(profile: Uuid, kind: Kind) -> String {
    format!("dev.ferrum.Rdp/{}/{profile}", kind.as_str())
}

#[cfg(windows)]
pub async fn store(profile: Uuid, kind: Kind, label: &str, password: &str) -> Result<()> {
    use anyhow::Context;
    wincred::write(&target_name(profile, kind), label, password.as_bytes())
        .context("store password in Credential Manager")
}

#[cfg(windows)]
pub async fn lookup(profile: Uuid, kind: Kind) -> Result<Option<String>> {
    use anyhow::Context;
    let secret = wincred::read(&target_name(profile, kind))
        .context("read password from Credential Manager")?;
    Ok(secret.map(|s| String::from_utf8_lossy(&s).into_owned()))
}

#[cfg(windows)]
pub async fn delete(profile: Uuid, kind: Kind) -> Result<()> {
    use anyhow::Context;
    wincred::delete(&target_name(profile, kind))
        .context("delete password from Credential Manager")
}

// No keyring elsewhere: passwords are simply not remembered.

#[cfg(not(any(target_os = "linux", windows)))]
pub async fn store(_profile: Uuid, _kind: Kind, _label: &str, _password: &str) -> Result<()> {
    anyhow::bail!("no keyring on this platform")
}

#[cfg(not(any(target_os = "linux", windows)))]
pub async fn lookup(_profile: Uuid, _kind: Kind) -> Result<Option<String>> {
    Ok(None)
}

#[cfg(not(any(target_os = "linux", windows)))]
pub async fn delete(_profile: Uuid, _kind: Kind) -> Result<()> {
    Ok(())
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    fn run<F: std::future::Future>(f: F) -> F::Output {
        gtk::glib::MainContext::default().block_on(f)
    }

    #[test]
    fn credential_manager_roundtrip() {
        let id = Uuid::new_v4();
        run(store(id, Kind::Session, "ferrum test", "pä$$wörd")).unwrap();
        assert_eq!(run(lookup(id, Kind::Session)).unwrap().as_deref(), Some("pä$$wörd"));
        assert_eq!(run(lookup(id, Kind::Gateway)).unwrap(), None);
        run(delete(id, Kind::Session)).unwrap();
        assert_eq!(run(lookup(id, Kind::Session)).unwrap(), None);
        run(delete(id, Kind::Session)).unwrap();
    }
}
