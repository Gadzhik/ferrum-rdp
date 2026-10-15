//! Passwords in the Secret Service (GNOME Keyring, KWallet, KeePassXC...).
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

// No Secret Service elsewhere: passwords are simply not remembered.

#[cfg(not(target_os = "linux"))]
pub async fn store(_profile: Uuid, _kind: Kind, _label: &str, _password: &str) -> Result<()> {
    anyhow::bail!("no keyring on this platform")
}

#[cfg(not(target_os = "linux"))]
pub async fn lookup(_profile: Uuid, _kind: Kind) -> Result<Option<String>> {
    Ok(None)
}

#[cfg(not(target_os = "linux"))]
pub async fn delete(_profile: Uuid, _kind: Kind) -> Result<()> {
    Ok(())
}
