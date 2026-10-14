//! Microsoft `.rdp` file import/export.
//!
//! Lines are `key:type:value` with type `s` (string), `i` (integer) or
//! `b` (binary). Files written by mstsc are UTF-16LE with a BOM.

use std::collections::BTreeMap;

use crate::model::*;

/// Decode raw file bytes (UTF-16LE/BE with BOM, or UTF-8 with optional BOM).
pub fn decode(bytes: &[u8]) -> String {
    let utf16 = |le: bool, body: &[u8]| {
        let units: Vec<u16> = body
            .chunks_exact(2)
            .map(|c| {
                if le {
                    u16::from_le_bytes([c[0], c[1]])
                } else {
                    u16::from_be_bytes([c[0], c[1]])
                }
            })
            .collect();
        String::from_utf16_lossy(&units)
    };
    match bytes {
        [0xFF, 0xFE, rest @ ..] => utf16(true, rest),
        [0xFE, 0xFF, rest @ ..] => utf16(false, rest),
        [0xEF, 0xBB, 0xBF, rest @ ..] => String::from_utf8_lossy(rest).into_owned(),
        // UTF-16LE without BOM: every second byte of ASCII text is zero
        [a, 0, ..] if *a != 0 => utf16(true, bytes),
        _ => String::from_utf8_lossy(bytes).into_owned(),
    }
}

/// Parse into a key → value map (keys lowercased, later keys win).
pub fn parse_entries(text: &str) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    for line in text.lines() {
        let line = line.trim_matches(|c: char| c.is_whitespace() || c == '\u{feff}');
        let mut parts = line.splitn(3, ':');
        let (Some(key), Some(kind), Some(value)) = (parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        if !matches!(kind, "s" | "i" | "b") {
            continue;
        }
        map.insert(key.trim().to_lowercase(), value.to_string());
    }
    map
}

fn int(map: &BTreeMap<String, String>, key: &str) -> Option<i64> {
    map.get(key).and_then(|v| v.trim().parse().ok())
}

fn flag(map: &BTreeMap<String, String>, key: &str) -> Option<bool> {
    int(map, key).map(|v| v != 0)
}

fn string(map: &BTreeMap<String, String>, key: &str) -> Option<String> {
    map.get(key)
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

/// Split `host[:port]`, handling `[v6]:port`.
pub fn split_host_port(addr: &str) -> (String, Option<u16>) {
    let addr = addr.trim();
    if let Some(rest) = addr.strip_prefix('[')
        && let Some((h, tail)) = rest.split_once(']')
    {
        let port = tail.strip_prefix(':').and_then(|p| p.parse().ok());
        return (h.to_string(), port);
    }
    match addr.rsplit_once(':') {
        // a bare IPv6 address has several colons and no port
        Some((h, p)) if !h.contains(':') => match p.parse() {
            Ok(port) => (h.to_string(), Some(port)),
            Err(_) => (addr.to_string(), None),
        },
        _ => (addr.to_string(), None),
    }
}

pub fn import(text: &str, name_hint: &str) -> Profile {
    let m = parse_entries(text);
    let mut p = Profile {
        name: name_hint.to_string(),
        ..Default::default()
    };

    let address = string(&m, "full address")
        .or_else(|| string(&m, "alternate full address"))
        .unwrap_or_default();
    let (host, port) = split_host_port(&address);
    p.host = host;
    p.port = port
        .or_else(|| int(&m, "server port").map(|v| v as u16))
        .unwrap_or(3389);

    if let Some(user) = string(&m, "username") {
        if let Some((d, u)) = user.split_once('\\') {
            p.domain = d.into();
            p.username = u.into();
        } else {
            p.username = user;
        }
    }
    if let Some(d) = string(&m, "domain") {
        p.domain = d;
    }

    // display
    if int(&m, "screen mode id") == Some(2) {
        p.display.mode = WindowMode::Fullscreen;
    }
    let dynamic = flag(&m, "dynamic resolution").unwrap_or(true);
    if let (Some(w), Some(h)) = (int(&m, "desktopwidth"), int(&m, "desktopheight"))
        && !dynamic
        && w > 0
        && h > 0
    {
        p.display.resolution = Resolution::Fixed {
            width: w as u32,
            height: h as u32,
        };
    }
    p.display.multimon = flag(&m, "use multimon").unwrap_or(false);
    if let Some(bpp) = int(&m, "session bpp") {
        p.display.color_depth = bpp as u32;
    }
    if let Some(scale) = int(&m, "desktopscalefactor") {
        p.display.scale = scale as u32;
    }

    // security
    if int(&m, "authentication level") == Some(0) {
        p.security.ignore_certificate = true;
    }
    if flag(&m, "enablecredsspsupport") == Some(false) {
        p.security.protocol = SecurityProtocol::Tls;
    }
    if flag(&m, "enablerdsaadauth") == Some(true) {
        p.security.protocol = SecurityProtocol::Aad;
    }
    p.security.admin = flag(&m, "administrative session")
        .or_else(|| flag(&m, "connect to console"))
        .unwrap_or(false);
    p.security.restricted_admin = flag(&m, "restricted admin mode").unwrap_or(false);
    p.security.load_balance_info = string(&m, "loadbalanceinfo").unwrap_or_default();
    p.security.kdc_url = string(&m, "kdcproxyname").unwrap_or_default();

    // gateway
    if let Some(gw) = string(&m, "gatewayhostname") {
        let (h, port) = split_host_port(&gw);
        p.gateway.host = h;
        p.gateway.port = port.unwrap_or(443);
        p.gateway.usage = match int(&m, "gatewayusagemethod") {
            Some(0) | Some(4) => GatewayUsage::Never,
            Some(2) => GatewayUsage::Detect,
            _ => GatewayUsage::Always,
        };
        p.gateway.same_credentials = flag(&m, "promptcredentialonce").unwrap_or(true);
    }

    // resources
    let r = &mut p.resources;
    if flag(&m, "redirectclipboard") == Some(false) {
        r.clipboard = ClipboardDirection::Off;
    }
    r.audio = match int(&m, "audiomode") {
        Some(1) => AudioMode::Remote,
        Some(2) => AudioMode::Off,
        _ => AudioMode::Local,
    };
    r.microphone = flag(&m, "audiocapturemode").unwrap_or(false);
    r.printers = flag(&m, "redirectprinters").unwrap_or(false);
    r.smartcards = flag(&m, "redirectsmartcards").unwrap_or(false);
    r.serial_ports = flag(&m, "redirectcomports").unwrap_or(false);
    if let Some(drives) = string(&m, "drivestoredirect") {
        // Windows drive letters do not exist here; "*" means everything.
        if drives.contains('*') || drives.to_lowercase().contains("dynamicdrives") {
            r.removable_drives = true;
            r.home_folder = true;
        }
    }
    if string(&m, "usbdevicestoredirect").is_some_and(|v| v.contains('*')) {
        r.usb_auto = true;
    }
    if string(&m, "camerastoredirect").is_some_and(|v| v.contains('*')) {
        r.webcam = true;
    }

    // performance
    let perf = &mut p.performance;
    perf.network = match int(&m, "connection type") {
        Some(1) => NetworkProfile::Modem,
        Some(2) => NetworkProfile::BroadbandLow,
        Some(3) => NetworkProfile::Wan, // satellite
        Some(4) => NetworkProfile::BroadbandHigh,
        Some(5) => NetworkProfile::Wan,
        Some(6) => NetworkProfile::Lan,
        _ => NetworkProfile::Auto,
    };
    if let Some(v) = flag(&m, "disable wallpaper") {
        perf.wallpaper = !v;
    }
    if let Some(v) = flag(&m, "disable themes") {
        perf.themes = !v;
    }
    if let Some(v) = flag(&m, "disable full window drag") {
        perf.window_drag = !v;
    }
    if let Some(v) = flag(&m, "disable menu anims") {
        perf.menu_animations = !v;
    }
    if let Some(v) = flag(&m, "allow font smoothing") {
        perf.font_smoothing = v;
    }
    if let Some(v) = flag(&m, "allow desktop composition") {
        perf.desktop_composition = v;
    }
    if let Some(v) = flag(&m, "compression") {
        perf.compression = v;
    }
    if let Some(v) = flag(&m, "autoreconnection enabled") {
        perf.auto_reconnect = v;
    }

    // RemoteApp
    if flag(&m, "remoteapplicationmode") == Some(true) {
        p.remote_app.enabled = true;
        p.remote_app.program = string(&m, "remoteapplicationprogram").unwrap_or_default();
        p.remote_app.name = string(&m, "remoteapplicationname").unwrap_or_default();
        p.remote_app.args = string(&m, "remoteapplicationcmdline").unwrap_or_default();
        p.remote_app.workdir = string(&m, "shell working directory").unwrap_or_default();
        if p.name.is_empty() && !p.remote_app.name.is_empty() {
            p.name = p.remote_app.name.clone();
        }
    }

    // input
    if let Some(hook) = int(&m, "keyboardhook") {
        p.input.grab_keyboard = hook != 0;
    }

    if p.name.is_empty() {
        p.name = p.address();
    }
    p
}

/// Render as `.rdp` text (CRLF line endings).
pub fn export(p: &Profile) -> String {
    let mut lines: Vec<String> = Vec::new();
    let mut s = |k: &str, v: &str| lines.push(format!("{k}:s:{v}"));
    s("full address", &p.address());
    if !p.username.is_empty() {
        let user = if p.domain.is_empty() {
            p.username.clone()
        } else {
            format!("{}\\{}", p.domain, p.username)
        };
        s("username", &user);
    }
    if p.gateway.enabled() {
        let gw = if p.gateway.port == 443 {
            p.gateway.host.clone()
        } else {
            format!("{}:{}", p.gateway.host, p.gateway.port)
        };
        s("gatewayhostname", &gw);
    }
    if !p.security.load_balance_info.is_empty() {
        s("loadbalanceinfo", &p.security.load_balance_info);
    }
    if !p.security.kdc_url.is_empty() {
        s("kdcproxyname", &p.security.kdc_url);
    }
    if p.remote_app.enabled {
        s("remoteapplicationprogram", &p.remote_app.program);
        s("remoteapplicationname", &p.remote_app.name);
        s("remoteapplicationcmdline", &p.remote_app.args);
        s("shell working directory", &p.remote_app.workdir);
    }
    if p.resources.removable_drives || p.resources.home_folder || !p.resources.folders.is_empty() {
        s("drivestoredirect", "*");
    }
    if p.resources.usb_auto {
        s("usbdevicestoredirect", "*");
    }
    if p.resources.webcam {
        s("camerastoredirect", "*");
    }

    let mut i = |k: &str, v: i64| lines.push(format!("{k}:i:{v}"));
    let b = |v: bool| v as i64;
    i(
        "screen mode id",
        if p.display.mode == WindowMode::Fullscreen {
            2
        } else {
            1
        },
    );
    match p.display.resolution {
        Resolution::Dynamic => i("dynamic resolution", 1),
        Resolution::Fixed { width, height } => {
            i("dynamic resolution", 0);
            i("desktopwidth", width as i64);
            i("desktopheight", height as i64);
        }
    }
    i("use multimon", b(p.display.multimon));
    i("session bpp", p.display.color_depth as i64);
    if p.display.scale != 0 {
        i("desktopscalefactor", p.display.scale as i64);
    }
    i(
        "authentication level",
        if p.security.ignore_certificate { 0 } else { 2 },
    );
    i(
        "enablecredsspsupport",
        b(!matches!(
            p.security.protocol,
            SecurityProtocol::Tls | SecurityProtocol::Rdp
        )),
    );
    if p.security.protocol == SecurityProtocol::Aad {
        i("enablerdsaadauth", 1);
    }
    i("administrative session", b(p.security.admin));
    if p.security.restricted_admin {
        i("restricted admin mode", 1);
    }
    if p.gateway.enabled() {
        i(
            "gatewayusagemethod",
            if p.gateway.usage == GatewayUsage::Detect {
                2
            } else {
                1
            },
        );
        i("gatewayprofileusagemethod", 1);
        i("promptcredentialonce", b(p.gateway.same_credentials));
    }
    i(
        "redirectclipboard",
        b(p.resources.clipboard != ClipboardDirection::Off),
    );
    i(
        "audiomode",
        match p.resources.audio {
            AudioMode::Local => 0,
            AudioMode::Remote => 1,
            AudioMode::Off => 2,
        },
    );
    i("audiocapturemode", b(p.resources.microphone));
    i("redirectprinters", b(p.resources.printers));
    i("redirectsmartcards", b(p.resources.smartcards));
    i("redirectcomports", b(p.resources.serial_ports));
    let perf = &p.performance;
    i(
        "connection type",
        match perf.network {
            NetworkProfile::Modem => 1,
            NetworkProfile::BroadbandLow => 2,
            NetworkProfile::Broadband | NetworkProfile::BroadbandHigh => 4,
            NetworkProfile::Wan => 5,
            NetworkProfile::Lan => 6,
            NetworkProfile::Auto => 7,
        },
    );
    i("networkautodetect", b(perf.network == NetworkProfile::Auto));
    i("bandwidthautodetect", 1);
    i("disable wallpaper", b(!perf.wallpaper));
    i("disable themes", b(!perf.themes));
    i("disable full window drag", b(!perf.window_drag));
    i("disable menu anims", b(!perf.menu_animations));
    i("allow font smoothing", b(perf.font_smoothing));
    i("allow desktop composition", b(perf.desktop_composition));
    i("compression", b(perf.compression));
    i("autoreconnection enabled", b(perf.auto_reconnect));
    i("remoteapplicationmode", b(p.remote_app.enabled));
    i("keyboardhook", if p.input.grab_keyboard { 1 } else { 0 });

    let mut out = lines.join("\r\n");
    out.push_str("\r\n");
    out
}

/// Encode as UTF-16LE with BOM, the way mstsc writes files.
pub fn encode_utf16(text: &str) -> Vec<u8> {
    let mut out = vec![0xFF, 0xFE];
    for unit in text.encode_utf16() {
        out.extend_from_slice(&unit.to_le_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "screen mode id:i:2\r\nuse multimon:i:1\r\ndesktopwidth:i:1920\r\ndesktopheight:i:1080\r\n\
session bpp:i:32\r\nfull address:s:rdp.example.com:3390\r\naudiomode:i:1\r\nredirectprinters:i:1\r\n\
redirectclipboard:i:1\r\nusername:s:CORP\\alice\r\ngatewayhostname:s:gw.example.com\r\ngatewayusagemethod:i:2\r\n\
remoteapplicationmode:i:1\r\nremoteapplicationprogram:s:||calc\r\nremoteapplicationname:s:Calculator\r\n\
dynamic resolution:i:0\r\nconnection type:i:6\r\ndisable wallpaper:i:1\r\n";

    #[test]
    fn imports_mstsc_file() {
        let p = import(SAMPLE, "");
        assert_eq!(p.host, "rdp.example.com");
        assert_eq!(p.port, 3390);
        assert_eq!(p.domain, "CORP");
        assert_eq!(p.username, "alice");
        assert_eq!(p.display.mode, WindowMode::Fullscreen);
        assert!(p.display.multimon);
        assert_eq!(
            p.display.resolution,
            Resolution::Fixed {
                width: 1920,
                height: 1080
            }
        );
        assert_eq!(p.resources.audio, AudioMode::Remote);
        assert!(p.resources.printers);
        assert_eq!(p.gateway.host, "gw.example.com");
        assert_eq!(p.gateway.usage, GatewayUsage::Detect);
        assert!(p.remote_app.enabled);
        assert_eq!(p.remote_app.program, "||calc");
        assert_eq!(p.name, "Calculator");
        assert_eq!(p.performance.network, NetworkProfile::Lan);
        assert!(!p.performance.wallpaper);
    }

    #[test]
    fn roundtrip_through_utf16() {
        let original = import(SAMPLE, "x");
        let bytes = encode_utf16(&export(&original));
        let again = import(&decode(&bytes), "x");
        assert_eq!(again.host, original.host);
        assert_eq!(again.port, original.port);
        assert_eq!(again.username, original.username);
        assert_eq!(again.domain, original.domain);
        assert_eq!(again.display, original.display);
        assert_eq!(again.gateway.host, original.gateway.host);
        assert_eq!(again.gateway.usage, original.gateway.usage);
        assert_eq!(again.remote_app, original.remote_app);
        assert_eq!(again.resources.audio, original.resources.audio);
        assert_eq!(again.performance, original.performance);
    }

    #[test]
    fn host_port_split() {
        assert_eq!(split_host_port("h"), ("h".into(), None));
        assert_eq!(split_host_port("h:1"), ("h".into(), Some(1)));
        assert_eq!(split_host_port("[::1]:5"), ("::1".into(), Some(5)));
        assert_eq!(split_host_port("fe80::1"), ("fe80::1".into(), None));
    }

    #[test]
    fn decodes_bomless_utf16() {
        let bytes: Vec<u8> = "a:s:b"
            .encode_utf16()
            .flat_map(|u| u.to_le_bytes())
            .collect();
        assert_eq!(decode(&bytes), "a:s:b");
    }
}
