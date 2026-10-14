//! The generated arguments must be accepted by FreeRDP's own parser.

use ferrum_profiles::*;

fn assert_valid(p: &Profile, params: &ConnectParams) {
    let args = to_args(p, params);
    let config = rdp_core::Config {
        args: args.clone(),
        ..Default::default()
    };
    if let Err(e) = rdp_core::validate(&config) {
        panic!("FreeRDP rejected {:?}: {e}", redact(&args));
    }
}

#[test]
fn default_profile() {
    assert_valid(
        &Profile::new("x", "host.example.com"),
        &ConnectParams::default(),
    );
}

#[test]
fn everything_enabled() {
    let mut p = Profile::new("x", "fe80::1");
    p.port = 3390;
    p.username = "user".into();
    p.domain = "CORP".into();
    p.display.mode = WindowMode::Fullscreen;
    p.display.multimon = true;
    p.display.scale = 150;
    p.display.color_depth = 24;
    p.security.protocol = SecurityProtocol::Nla;
    p.security.admin = true;
    p.security.kdc_url = "https://kdc.example.com/KdcProxy".into();
    p.security.load_balance_info = "tsv://MS Terminal Services Plugin.1.Coll".into();
    p.gateway.usage = GatewayUsage::Detect;
    p.gateway.host = "gw.example.com".into();
    p.gateway.port = 8443;
    p.gateway.transport = GatewayTransport::Http;
    p.gateway.same_credentials = false;
    p.gateway.username = "gwuser".into();
    p.resources.clipboard = ClipboardDirection::ToRemote;
    p.resources.audio = AudioMode::Local;
    p.resources.microphone = true;
    p.resources.folders.push(SharedFolder {
        name: "My Docs".into(),
        path: "/tmp".into(),
    });
    p.resources.home_folder = true;
    p.resources.removable_drives = true;
    p.resources.printers = true;
    p.resources.smartcards = true;
    p.resources.usb_devices = vec!["046d:c52b".into()];
    p.resources.ssh_agent = true;
    p.performance.network = NetworkProfile::BroadbandHigh;
    p.performance.codec = Codec::Avc444;
    p.performance.wallpaper = false;
    p.performance.udp = false;
    p.remote_app = RemoteApp {
        enabled: true,
        program: "||notepad".into(),
        name: "Notepad".into(),
        args: "C:\\a, b.txt".into(),
        workdir: "C:\\".into(),
    };
    p.input.relative_mouse = true;
    p.advanced.client_hostname = "mybox".into();
    let params = ConnectParams {
        password: Some("p@ss,word".into()),
        gateway_password: Some("gw".into()),
        initial_size: Some((1600, 900)),
        local_scale: 125,
        keyboard_layout: Some(0x409),
        ..Default::default()
    };
    assert_valid(&p, &params);
}

#[test]
fn each_codec_and_network() {
    for codec in [
        Codec::Auto,
        Codec::Avc420,
        Codec::RemoteFx,
        Codec::Progressive,
        Codec::Legacy,
    ] {
        let mut p = Profile::new("x", "h");
        p.performance.codec = codec;
        p.display.resolution = Resolution::Fixed {
            width: 1024,
            height: 768,
        };
        p.resources.audio = AudioMode::Remote;
        p.resources.clipboard = ClipboardDirection::Off;
        p.resources.usb_auto = true;
        p.security.protocol = SecurityProtocol::Tls;
        p.gateway.usage = GatewayUsage::Always;
        p.gateway.host = "gw".into();
        p.gateway.transport = GatewayTransport::Rpc;
        assert_valid(&p, &ConnectParams::default());
    }
}

#[test]
fn bogus_argument_is_rejected() {
    let config = rdp_core::Config {
        args: vec!["/v:h".into(), "/no-such-option:1".into()],
        ..Default::default()
    };
    assert!(rdp_core::validate(&config).is_err());
}
