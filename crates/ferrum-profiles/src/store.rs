//! Profile persistence in `~/.config/ferrum/connections.toml`.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::model::Profile;

#[derive(Debug, Default, Serialize, Deserialize)]
struct File {
    #[serde(default, rename = "connection")]
    profiles: Vec<Profile>,
}

#[derive(Debug)]
pub struct Store {
    path: PathBuf,
    profiles: Vec<Profile>,
}

pub fn config_dir() -> PathBuf {
    directories::ProjectDirs::from("dev", "ferrum", "ferrum")
        .map(|d| d.config_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from(".ferrum"))
}

impl Store {
    pub fn default_path() -> PathBuf {
        config_dir().join("connections.toml")
    }

    /// Load from `path`; a missing file is an empty store.
    pub fn load(path: impl Into<PathBuf>) -> Result<Store> {
        let path = path.into();
        let profiles = match std::fs::read_to_string(&path) {
            Ok(text) => {
                toml::from_str::<File>(&text)
                    .with_context(|| format!("parse {}", path.display()))?
                    .profiles
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(e).with_context(|| format!("read {}", path.display())),
        };
        Ok(Store { path, profiles })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Write atomically (temp file + rename), user-only permissions.
    pub fn save(&self) -> Result<()> {
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let text = toml::to_string_pretty(&File {
            profiles: self.profiles.clone(),
        })?;
        let tmp = self.path.with_extension("toml.tmp");
        {
            use std::io::Write;
            #[cfg(unix)]
            use std::os::unix::fs::OpenOptionsExt;
            let mut opts = std::fs::OpenOptions::new();
            opts.write(true).create(true).truncate(true);
            #[cfg(unix)]
            opts.mode(0o600);
            let mut f = opts.open(&tmp)?;
            f.write_all(text.as_bytes())?;
            f.sync_all()?;
        }
        std::fs::rename(&tmp, &self.path)?;
        Ok(())
    }

    pub fn profiles(&self) -> &[Profile] {
        &self.profiles
    }

    pub fn get(&self, id: Uuid) -> Option<&Profile> {
        self.profiles.iter().find(|p| p.id == id)
    }

    /// Insert or replace by id.
    pub fn upsert(&mut self, profile: Profile) {
        match self.profiles.iter_mut().find(|p| p.id == profile.id) {
            Some(slot) => *slot = profile,
            None => self.profiles.push(profile),
        }
    }

    pub fn remove(&mut self, id: Uuid) -> Option<Profile> {
        let idx = self.profiles.iter().position(|p| p.id == id)?;
        Some(self.profiles.remove(idx))
    }

    pub fn touch(&mut self, id: Uuid, timestamp: i64) {
        if let Some(p) = self.profiles.iter_mut().find(|p| p.id == id) {
            p.last_connected = Some(timestamp);
        }
    }

    /// All distinct group paths including parents, sorted.
    pub fn groups(&self) -> Vec<String> {
        let mut groups: Vec<String> = Vec::new();
        for p in &self.profiles {
            let mut acc = String::new();
            for part in p.group.split('/').filter(|s| !s.is_empty()) {
                if !acc.is_empty() {
                    acc.push('/');
                }
                acc.push_str(part);
                if !groups.contains(&acc) {
                    groups.push(acc.clone());
                }
            }
        }
        groups.sort_by_key(|g| g.to_lowercase());
        groups
    }

    /// Profiles matching `query`, favorites first, then by name.
    pub fn search(&self, query: &str, group: Option<&str>) -> Vec<&Profile> {
        let mut out: Vec<&Profile> = self
            .profiles
            .iter()
            .filter(|p| p.matches(query))
            .filter(|p| match group {
                None => true,
                Some(g) => p.group == g || p.group.starts_with(&format!("{g}/")),
            })
            .collect();
        out.sort_by(|a, b| {
            b.favorite.cmp(&a.favorite).then_with(|| {
                a.display_name()
                    .to_lowercase()
                    .cmp(&b.display_name().to_lowercase())
            })
        });
        out
    }

    /// Most recently used first.
    pub fn recent(&self, limit: usize) -> Vec<&Profile> {
        let mut out: Vec<&Profile> = self
            .profiles
            .iter()
            .filter(|p| p.last_connected.is_some())
            .collect();
        out.sort_by(|a, b| b.last_connected.cmp(&a.last_connected));
        out.truncate(limit);
        out
    }

    /// Find a profile by exact name or host (for `ferrum --profile NAME`).
    pub fn find_by_name(&self, name: &str) -> Option<&Profile> {
        let n = name.to_lowercase();
        self.profiles
            .iter()
            .find(|p| p.name.to_lowercase() == n)
            .or_else(|| self.profiles.iter().find(|p| p.host.to_lowercase() == n))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::*;

    #[test]
    fn save_and_load_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub/connections.toml");
        let mut store = Store::load(&path).unwrap();
        assert!(store.profiles().is_empty());

        let mut p = Profile::new("Office", "office.example.com");
        p.group = "Work/HQ".into();
        p.resources.folders.push(SharedFolder {
            name: "docs".into(),
            path: "/tmp".into(),
        });
        p.display.resolution = Resolution::Fixed {
            width: 1600,
            height: 900,
        };
        let id = p.id;
        store.upsert(p.clone());
        store.save().unwrap();

        let loaded = Store::load(&path).unwrap();
        assert_eq!(loaded.get(id), Some(&p));
        assert_eq!(
            loaded.groups(),
            vec!["Work".to_string(), "Work/HQ".to_string()]
        );

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[test]
    fn search_orders_favorites_first() {
        let mut store = Store {
            path: PathBuf::new(),
            profiles: vec![],
        };
        let a = Profile::new("beta", "b");
        let mut b = Profile::new("alpha", "a");
        let mut c = Profile::new("zeta", "z");
        c.favorite = true;
        b.group = "Lab".into();
        store.upsert(a);
        store.upsert(b);
        store.upsert(c);
        let names: Vec<_> = store
            .search("", None)
            .iter()
            .map(|p| p.name.clone())
            .collect();
        assert_eq!(names, ["zeta", "alpha", "beta"]);
        assert_eq!(store.search("", Some("Lab")).len(), 1);
        assert_eq!(store.search("ALP", None)[0].name, "alpha");
    }

    #[test]
    fn missing_fields_use_defaults() {
        let text = "[[connection]]\nname = \"min\"\nhost = \"h\"\n";
        let f: super::File = toml::from_str(text).unwrap();
        let p = &f.profiles[0];
        assert_eq!(p.port, 3389);
        assert!(p.performance.auto_reconnect);
        assert_eq!(p.display.resolution, Resolution::Dynamic);
    }
}
