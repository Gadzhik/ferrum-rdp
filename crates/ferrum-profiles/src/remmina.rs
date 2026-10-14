//! Import RDP profiles from Remmina (`~/.local/share/remmina/*.remmina`).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::model::*;
use crate::rdpfile::split_host_port;

fn parse_ini(text: &str) -> HashMap<String, String> {
    let mut section = String::new();
    let mut map = HashMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('#') || line.starts_with(';') || line.is_empty() {
            continue;
        }
        if let Some(s) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            section = s.to_string();
            continue;
        }
        if section == "remmina" {
            if let Some((k, v)) = line.split_once('=') {
                map.insert(k.trim().to_string(), v.trim().to_string());
            }
        }
    }
    map
}

/// Parse one `.remmina` file. Returns `None` for non-RDP profiles.
pub fn import(text: &str) -> Option<Profile> {
    let m = parse_ini(text);
    let get = |k: &str| m.get(k).map(String::as_str).unwrap_or("");
    let on = |k: &str| matches!(get(k), "1" | "true");
    if !get("protocol").eq_ignore_ascii_case("RDP") {
        return None;
    }

    let (host, port) = split_host_port(get("server"));
    let mut p = Profile {
        name: get("name").to_string(),
        group: get("group").to_string(),
        host,
        port: port.unwrap_or(3389),
        username: get("username").to_string(),
        domain: get("domain").to_string(),
        ..Default::default()
    };

    // viewmode: 1 scrolled window, 2/3/4 fullscreen variants
    if matches!(get("viewmode"), "2" | "3" | "4") {
        p.display.mode = WindowMode::Fullscreen;
    }
    // resolution_mode: 0/1 = use client resolution, 2 = custom
    if get("resolution_mode") == "2" {
        if let (Ok(w), Ok(h)) = (
            get("resolution_width").parse(),
            get("resolution_height").parse(),
        ) {
            p.display.resolution = Resolution::Fixed {
                width: w,
                height: h,
            };
        }
    }
    if let Ok(bpp) = get("colordepth").parse::<u32>() {
        if matches!(bpp, 8 | 15 | 16 | 24 | 32) {
            p.display.color_depth = bpp;
        }
    }
    p.display.multimon = on("multimon");

    p.security.protocol = match get("security") {
        "nla" => SecurityProtocol::Nla,
        "tls" => SecurityProtocol::Tls,
        "rdp" => SecurityProtocol::Rdp,
        "ext" => SecurityProtocol::Ext,
        _ => SecurityProtocol::Auto,
    };
    p.security.ignore_certificate = on("cert_ignore");
    p.security.admin = on("console");
    p.security.load_balance_info = get("loadbalanceinfo").to_string();

    if !get("gateway_server").is_empty() {
        let (gh, gp) = split_host_port(get("gateway_server"));
        p.gateway.host = gh;
        p.gateway.port = gp.unwrap_or(443);
        p.gateway.usage = if on("gateway_usage") {
            GatewayUsage::Detect
        } else {
            GatewayUsage::Always
        };
        p.gateway.username = get("gateway_username").to_string();
        p.gateway.domain = get("gateway_domain").to_string();
        p.gateway.same_credentials = p.gateway.username.is_empty();
    }

    let r = &mut p.resources;
    if on("disableclipboard") {
        r.clipboard = ClipboardDirection::Off;
    }
    r.audio = match get("sound") {
        s if s.starts_with("local") => AudioMode::Local,
        "remote" => AudioMode::Remote,
        _ => AudioMode::Off,
    };
    r.microphone = on("microphone") || get("microphone").starts_with("sys:");
    if !get("sharefolder").is_empty() {
        r.folders.push(SharedFolder {
            name: String::new(),
            path: get("sharefolder").to_string(),
        });
    }
    r.printers = on("shareprinter");
    r.smartcards = on("sharesmartcard");
    r.serial_ports = on("shareserial");

    if !get("exec").is_empty() {
        p.remote_app.enabled = true;
        p.remote_app.program = get("exec").to_string();
        p.remote_app.workdir = get("execpath").to_string();
    }

    if on("ssh_tunnel_enabled") {
        let user = get("ssh_tunnel_username");
        let (sh, sp) = split_host_port(get("ssh_tunnel_server"));
        p.ssh.enabled = true;
        p.ssh.host = if user.is_empty() {
            sh
        } else {
            format!("{user}@{sh}")
        };
        p.ssh.port = sp.unwrap_or(22);
        p.ssh.identity_file = get("ssh_tunnel_privatekey").to_string();
    }

    if p.name.is_empty() {
        p.name = p.address();
    }
    Some(p)
}

pub fn default_dir() -> Option<PathBuf> {
    directories::BaseDirs::new().map(|b| b.data_dir().join("remmina"))
}

/// Import every RDP profile from a Remmina data directory.
pub fn import_dir(dir: &Path) -> Vec<Profile> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<Profile> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "remmina"))
        .filter_map(|p| std::fs::read_to_string(p).ok())
        .filter_map(|t| import(&t))
        .collect();
    out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn imports_rdp_profile() {
        let text = "[remmina]\nname=Office PC\nprotocol=RDP\nserver=10.0.0.5:3390\nusername=bob\n\
domain=CORP\ngroup=Work\nsound=local\nsharefolder=/home/bob/share\nviewmode=4\nsecurity=nla\n\
ssh_tunnel_enabled=1\nssh_tunnel_server=bastion.example.com\nssh_tunnel_username=bob\n";
        let p = import(text).unwrap();
        assert_eq!(p.name, "Office PC");
        assert_eq!(p.host, "10.0.0.5");
        assert_eq!(p.port, 3390);
        assert_eq!(p.group, "Work");
        assert_eq!(p.display.mode, WindowMode::Fullscreen);
        assert_eq!(p.security.protocol, SecurityProtocol::Nla);
        assert_eq!(p.resources.folders[0].path, "/home/bob/share");
        assert!(p.ssh.enabled);
        assert_eq!(p.ssh.host, "bob@bastion.example.com");
    }

    #[test]
    fn skips_other_protocols() {
        assert!(import("[remmina]\nprotocol=VNC\nserver=h\n").is_none());
    }
}
