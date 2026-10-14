//! Passwords in the Secret Service (GNOME Keyring, KWallet, KeePassXC...).

use std::collections::HashMap;

use anyhow::{Context, Result};
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

pub async fn store(profile: Uuid, kind: Kind, label: &str, password: &str) -> Result<()> {
    let keyring = oo7::Keyring::new().await.context("open keyring")?;
    let attrs = attributes(profile, kind);
    keyring
        .create_item(label, &as_str_map(&attrs), password.as_bytes(), true)
        .await
        .context("store password")?;
    Ok(())
}

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

pub async fn delete(profile: Uuid, kind: Kind) -> Result<()> {
    let keyring = oo7::Keyring::new().await.context("open keyring")?;
    let attrs = attributes(profile, kind);
    keyring
        .delete(&as_str_map(&attrs))
        .await
        .context("delete password")?;
    Ok(())
}
