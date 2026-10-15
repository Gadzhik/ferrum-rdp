//! Translate a [`Profile`] into FreeRDP 3 command line arguments.
//!
//! The arguments are parsed in-process by `freerdp_client_settings_parse_command_line`
//! (never passed to a child process), so secrets may appear here.

use crate::model::*;

/// Runtime values not stored in the profile.
#[derive(Clone, Debug, Default)]
pub struct ConnectParams {
    pub password: Option<String>,
    pub gateway_password: Option<String>,
    /// Overrides host:port, e.g. a local SSH tunnel endpoint.
    pub endpoint: Option<(String, u16)>,
    /// Initial size for dynamic resolution (the window size).
    pub initial_size: Option<(u32, u32)>,
    /// Local scale factor in percent, used when the profile says "auto".
    pub local_scale: u32,
    /// Keyboard layout id derived from the local layout.
    pub keyboard_layout: Option<u32>,
}

fn bool_flag(args: &mut Vec<String>, name: &str, on: bool) {
    args.push(format!("{}{name}", if on { '+' } else { '-' }));
}

/// Quote one comma-separated sub-option for FreeRDP.
///
/// FreeRDP only accepts quotes around a whole element (`"cmd:a, b"`, not
/// `cmd:"a, b"`), and a backslash escapes the next character, so a trailing
/// one (`C:\`) would swallow the separator. Doubling it keeps Windows paths
/// valid.
fn esc(v: &str) -> String {
    let mut s = v.to_string();
    if s.chars().rev().take_while(|&c| c == '\\').count() % 2 == 1 {
        s.push('\\');
    }
    if s.contains(',') {
        format!("\"{s}\"")
    } else {
        s
    }
}

/// A `key:value` sub-option.
fn opt(key: &str, v: &str) -> String {
    esc(&format!("{key}:{v}"))
}

/// Snap a percent scale to the device scale factors RDP knows about.
pub fn device_scale(percent: u32) -> u32 {
    match percent {
        0..=119 => 100,
        120..=159 => 140,
        _ => 180,
    }
}

pub fn to_args(p: &Profile, params: &ConnectParams) -> Vec<String> {
    let mut a: Vec<String> = Vec::new();

    // --- target ---
    let (host, port) = match &params.endpoint {
        Some((h, port)) => (h.clone(), *port),
        None => (p.host.trim().to_string(), p.port),
    };
    let host = if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]")
    } else {
        host
    };
    if port == 3389 || port == 0 {
        a.push(format!("/v:{host}"));
    } else {
        a.push(format!("/v:{host}:{port}"));
    }
    if !p.username.is_empty() {
        a.push(format!("/u:{}", p.username));
    }
    if !p.domain.is_empty() {
        a.push(format!("/d:{}", p.domain));
    }
    if let Some(pw) = &params.password {
        a.push(format!("/p:{pw}"));
    }

    // --- display ---
    let scale = if p.display.scale == 0 {
        params.local_scale.max(100)
    } else {
        p.display.scale
    };
    if p.display.mode == WindowMode::Fullscreen {
        a.push("+f".into());
    }
    if p.display.multimon {
        a.push("/multimon".into());
    }
    match p.display.resolution {
        Resolution::Dynamic => {
            a.push("+dynamic-resolution".into());
            if let Some((w, h)) = params.initial_size {
                a.push(format!("/size:{}x{}", w.max(200) & !1, h.max(200)));
            }
        }
        Resolution::Fixed { width, height } => {
            a.push(format!("/size:{width}x{height}"));
        }
    }
    if scale != 100 {
        a.push(format!("/scale-desktop:{scale}"));
        a.push(format!("/scale-device:{}", device_scale(scale)));
    }
    if p.display.color_depth != 0 && p.display.color_depth != 32 {
        a.push(format!("/bpp:{}", p.display.color_depth));
    }

    // --- security ---
    match p.security.protocol {
        SecurityProtocol::Auto => {}
        SecurityProtocol::Nla => a.push("/sec:nla".into()),
        SecurityProtocol::Tls => a.push("/sec:tls".into()),
        SecurityProtocol::Rdp => a.push("/sec:rdp".into()),
        SecurityProtocol::Ext => a.push("/sec:ext".into()),
        SecurityProtocol::Aad => a.push("/sec:aad".into()),
    }
    if p.security.admin {
        a.push("+admin".into());
    }
    if p.security.restricted_admin {
        a.push("+restricted-admin".into());
    }
    if p.security.ignore_certificate {
        a.push("/cert:ignore".into());
    }
    if !p.security.kdc_url.is_empty() {
        a.push(format!("/kerberos:{}", opt("kdc-url", &p.security.kdc_url)));
    }
    if p.security.smartcard_logon {
        a.push("/smartcard-logon".into());
    }
    if !p.security.load_balance_info.is_empty() {
        a.push(format!(
            "/load-balance-info:{}",
            p.security.load_balance_info
        ));
    }

    // --- gateway ---
    if p.gateway.enabled() {
        let g = &p.gateway;
        let mut opts = vec![if g.port == 443 || g.port == 0 {
            format!("g:{}", g.host.trim())
        } else {
            format!("g:{}:{}", g.host.trim(), g.port)
        }];
        if g.same_credentials {
            if !p.username.is_empty() {
                opts.push(opt("u", &p.username));
            }
            if !p.domain.is_empty() {
                opts.push(opt("d", &p.domain));
            }
            if let Some(pw) = &params.password {
                opts.push(opt("p", pw));
            }
        } else {
            if !g.username.is_empty() {
                opts.push(opt("u", &g.username));
            }
            if !g.domain.is_empty() {
                opts.push(opt("d", &g.domain));
            }
            if let Some(pw) = &params.gateway_password {
                opts.push(opt("p", pw));
            }
        }
        opts.push(format!(
            "usage-method:{}",
            if g.usage == GatewayUsage::Detect {
                "detect"
            } else {
                "direct"
            }
        ));
        let ws = if g.websockets { "" } else { ",no-websockets" };
        opts.push(match g.transport {
            GatewayTransport::Auto => format!("type:auto{ws}"),
            GatewayTransport::Http => format!("type:http{ws}"),
            GatewayTransport::Rpc => "type:rpc".into(),
            GatewayTransport::Arm => "type:arm".into(),
        });
        a.push(format!("/gateway:{}", opts.join(",")));
    }

    // --- local resources ---
    let r = &p.resources;
    let dir = match r.clipboard {
        ClipboardDirection::Both => "all",
        ClipboardDirection::ToLocal => "local",
        ClipboardDirection::ToRemote => "remote",
        ClipboardDirection::Off => "off",
    };
    if r.clipboard == ClipboardDirection::Off {
        a.push("-clipboard".into());
    } else {
        let files = if r.clipboard_files { dir } else { "off" };
        a.push(format!("/clipboard:direction-to:{dir},files-to:{files}"));
    }
    match r.audio {
        AudioMode::Local => a.push("/sound:sys:pulse".into()),
        AudioMode::Remote => a.push("/audio-mode:1".into()),
        AudioMode::Off => a.push("/audio-mode:2".into()),
    }
    if r.microphone {
        a.push("/microphone:sys:pulse".into());
    }
    for f in &r.folders {
        if !f.path.is_empty() {
            let name = if f.name.is_empty() {
                std::path::Path::new(&f.path)
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "share".into())
            } else {
                f.name.clone()
            };
            a.push(format!("/drive:{},{}", esc(&name), esc(&f.path)));
        }
    }
    if r.home_folder {
        a.push("+home-drive".into());
    }
    if r.removable_drives {
        a.push("/drive:hotplug,*".into());
    }
    if r.printers {
        a.push("/printer".into());
    }
    if r.smartcards {
        a.push("/smartcard".into());
    }
    if r.usb_auto {
        a.push("/usb:auto".into());
    } else if !r.usb_devices.is_empty() {
        a.push(format!("/usb:id:{}", r.usb_devices.join("#")));
    }
    if r.webcam {
        a.push("/dvc:rdpecam".into());
    }
    if r.serial_ports {
        a.push("/serial".into());
    }
    if r.ssh_agent {
        a.push("+ssh-agent".into());
    }

    // --- performance ---
    let perf = &p.performance;
    a.push(format!(
        "/network:{}",
        match perf.network {
            NetworkProfile::Auto => "auto",
            NetworkProfile::Lan => "lan",
            NetworkProfile::BroadbandHigh => "broadband-high",
            NetworkProfile::Broadband => "broadband",
            NetworkProfile::BroadbandLow => "broadband-low",
            NetworkProfile::Wan => "wan",
            NetworkProfile::Modem => "modem",
        }
    ));
    match perf.codec {
        Codec::Auto => a.push("/gfx".into()),
        Codec::Avc444 => a.push("/gfx:AVC444".into()),
        Codec::Avc420 => a.push("/gfx:AVC420".into()),
        Codec::RemoteFx => a.push("/gfx:RFX".into()),
        Codec::Progressive => a.push("/gfx:progressive".into()),
        Codec::Legacy => a.push("-gfx".into()),
    }
    bool_flag(&mut a, "wallpaper", perf.wallpaper);
    bool_flag(&mut a, "themes", perf.themes);
    bool_flag(&mut a, "fonts", perf.font_smoothing);
    bool_flag(&mut a, "aero", perf.desktop_composition);
    bool_flag(&mut a, "window-drag", perf.window_drag);
    bool_flag(&mut a, "menu-anims", perf.menu_animations);
    if !perf.compression {
        a.push("-compression".into());
    }
    if perf.auto_reconnect {
        a.push("+auto-reconnect".into());
        a.push(format!(
            "/auto-reconnect-max-retries:{}",
            perf.reconnect_retries
        ));
    }
    if !perf.udp {
        a.push("-multitransport".into());
    }

    // --- RemoteApp ---
    let app = &p.remote_app;
    if app.enabled && !app.program.is_empty() {
        let mut opts = vec![opt("program", &app.program)];
        if !app.name.is_empty() {
            opts.push(opt("name", &app.name));
        }
        if !app.args.is_empty() {
            opts.push(opt("cmd", &app.args));
        }
        if !app.workdir.is_empty() {
            opts.push(opt("workdir", &app.workdir));
        }
        a.push(format!("/app:{}", opts.join(",")));
    }

    // --- input ---
    let layout = if p.input.keyboard_layout.is_empty() {
        params.keyboard_layout.map(|l| format!("0x{l:08X}"))
    } else {
        Some(p.input.keyboard_layout.clone())
    };
    if let Some(l) = layout {
        a.push(format!("/kbd:layout:{l}"));
    }
    if p.input.relative_mouse {
        a.push("+mouse-relative".into());
    }

    // --- advanced ---
    let adv = &p.advanced;
    if !adv.client_hostname.is_empty() {
        a.push(format!("/client-hostname:{}", adv.client_hostname));
    }
    if !adv.timezone.is_empty() {
        a.push(format!("/timezone:{}", adv.timezone));
    }
    if !adv.proxy.is_empty() {
        a.push(format!("/proxy:{}", adv.proxy));
    }
    if !adv.preconnection_blob.is_empty() {
        a.push(format!("/pcb:{}", adv.preconnection_blob));
    }
    a.extend(
        adv.extra_args
            .iter()
            .filter(|s| !s.trim().is_empty())
            .cloned(),
    );
    a
}

/// Arguments with secrets masked, for logs and the "copy command" action.
pub fn redact(args: &[String]) -> Vec<String> {
    args.iter()
        .map(|a| {
            if a.starts_with("/p:") {
                "/p:********".into()
            } else if a.starts_with("/gateway:") {
                split_options(a)
                    .into_iter()
                    .map(|o| {
                        if o.trim_start_matches('"').starts_with("p:") {
                            "p:********"
                        } else {
                            o
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(",")
            } else {
                a.clone()
            }
        })
        .collect()
}

/// Split on commas that are not inside double quotes.
fn split_options(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut quoted = false;
    for (i, c) in s.char_indices() {
        match c {
            '"' => quoted = !quoted,
            ',' if !quoted => {
                out.push(&s[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    out.push(&s[start..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn has(args: &[String], s: &str) -> bool {
        args.iter().any(|a| a == s)
    }

    #[test]
    fn basic_target() {
        let mut p = Profile::new("x", "srv.example.com");
        p.username = "bob".into();
        p.domain = "CORP".into();
        let a = to_args(
            &p,
            &ConnectParams {
                password: Some("pw".into()),
                ..Default::default()
            },
        );
        assert_eq!(a[0], "/v:srv.example.com");
        assert!(has(&a, "/u:bob"));
        assert!(has(&a, "/d:CORP"));
        assert!(has(&a, "/p:pw"));
        assert!(has(&a, "+dynamic-resolution"));
        assert!(has(&a, "/sound:sys:pulse"));
        assert!(has(&a, "/clipboard:direction-to:all,files-to:all"));
    }

    #[test]
    fn port_ipv6_and_endpoint_override() {
        let mut p = Profile::new("x", "fe80::1");
        p.port = 3390;
        assert_eq!(
            to_args(&p, &ConnectParams::default())[0],
            "/v:[fe80::1]:3390"
        );
        let a = to_args(
            &p,
            &ConnectParams {
                endpoint: Some(("127.0.0.1".into(), 40000)),
                ..Default::default()
            },
        );
        assert_eq!(a[0], "/v:127.0.0.1:40000");
    }

    #[test]
    fn gateway_with_same_credentials() {
        let mut p = Profile::new("x", "h");
        p.username = "u".into();
        p.gateway.usage = GatewayUsage::Always;
        p.gateway.host = "gw.example.com".into();
        let a = to_args(
            &p,
            &ConnectParams {
                password: Some("s,ecret".into()),
                ..Default::default()
            },
        );
        let gw = a.iter().find(|a| a.starts_with("/gateway:")).unwrap();
        assert_eq!(
            gw,
            "/gateway:g:gw.example.com,u:u,\"p:s,ecret\",usage-method:direct,type:auto"
        );
        let red = redact(&a);
        assert!(red.iter().all(|a| !a.contains("ecret")));
    }

    #[test]
    fn resources_and_remote_app() {
        let mut p = Profile::new("x", "h");
        p.resources.folders.push(SharedFolder {
            name: String::new(),
            path: "/home/me/Docs".into(),
        });
        p.resources.usb_devices = vec!["046d:c52b".into(), "1050:0407".into()];
        p.resources.audio = AudioMode::Off;
        p.resources.clipboard = ClipboardDirection::Off;
        p.remote_app = RemoteApp {
            enabled: true,
            program: "||excel".into(),
            args: "a.xlsx".into(),
            ..Default::default()
        };
        let a = to_args(&p, &ConnectParams::default());
        assert!(has(&a, "/drive:Docs,/home/me/Docs"));
        assert!(has(&a, "/usb:id:046d:c52b#1050:0407"));
        assert!(has(&a, "/audio-mode:2"));
        assert!(has(&a, "-clipboard"));
        assert!(has(&a, "/app:program:||excel,cmd:a.xlsx"));
    }

    #[test]
    fn scale_and_fixed_resolution() {
        let mut p = Profile::new("x", "h");
        p.display.resolution = Resolution::Fixed {
            width: 1920,
            height: 1080,
        };
        let a = to_args(
            &p,
            &ConnectParams {
                local_scale: 150,
                ..Default::default()
            },
        );
        assert!(has(&a, "/size:1920x1080"));
        assert!(has(&a, "/scale-desktop:150"));
        assert!(has(&a, "/scale-device:140"));
        assert!(!has(&a, "+dynamic-resolution"));
    }

    #[test]
    fn sub_option_quoting() {
        assert_eq!(opt("u", "bob"), "u:bob");
        assert_eq!(opt("cmd", "a, b"), "\"cmd:a, b\"");
        assert_eq!(opt("workdir", "C:\\"), "workdir:C:\\\\");
        assert_eq!(esc("C:\\\\"), "C:\\\\");
    }

    #[test]
    fn dynamic_initial_size_is_even() {
        let p = Profile::new("x", "h");
        let a = to_args(
            &p,
            &ConnectParams {
                initial_size: Some((1281, 721)),
                ..Default::default()
            },
        );
        assert!(has(&a, "/size:1280x721"));
    }
}
