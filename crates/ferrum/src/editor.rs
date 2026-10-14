//! Connection profile editor.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use ferrum_profiles::*;
use gtk::{gio, glib};

use crate::monitors;

type Model = Rc<RefCell<Profile>>;

pub struct EditorResult {
    pub profile: Profile,
    /// New password entered in the editor (None = unchanged).
    pub password: Option<String>,
    pub gateway_password: Option<String>,
}

fn entry(
    group: &adw::PreferencesGroup,
    model: &Model,
    title: &str,
    get: impl Fn(&Profile) -> String,
    set: impl Fn(&mut Profile, String) + 'static,
) -> adw::EntryRow {
    let row = adw::EntryRow::builder()
        .title(title)
        .text(get(&model.borrow()))
        .build();
    let m = model.clone();
    row.connect_changed(move |r| set(&mut m.borrow_mut(), r.text().to_string()));
    group.add(&row);
    row
}

fn switch(
    group: &adw::PreferencesGroup,
    model: &Model,
    title: &str,
    subtitle: &str,
    get: impl Fn(&Profile) -> bool,
    set: impl Fn(&mut Profile, bool) + 'static,
) -> adw::SwitchRow {
    let row = adw::SwitchRow::builder()
        .title(title)
        .active(get(&model.borrow()))
        .build();
    if !subtitle.is_empty() {
        row.set_subtitle(subtitle);
    }
    let m = model.clone();
    row.connect_active_notify(move |r| set(&mut m.borrow_mut(), r.is_active()));
    group.add(&row);
    row
}

fn combo(
    group: &adw::PreferencesGroup,
    model: &Model,
    title: &str,
    options: &[&str],
    get: impl Fn(&Profile) -> usize,
    set: impl Fn(&mut Profile, usize) + 'static,
) -> adw::ComboRow {
    let list = gtk::StringList::new(options);
    let row = adw::ComboRow::builder()
        .title(title)
        .model(&list)
        .selected(get(&model.borrow()) as u32)
        .build();
    let m = model.clone();
    row.connect_selected_notify(move |r| set(&mut m.borrow_mut(), r.selected() as usize));
    group.add(&row);
    row
}

fn spin(
    group: &adw::PreferencesGroup,
    model: &Model,
    title: &str,
    min: f64,
    max: f64,
    get: impl Fn(&Profile) -> u32,
    set: impl Fn(&mut Profile, u32) + 'static,
) -> adw::SpinRow {
    let row = adw::SpinRow::with_range(min, max, 1.0);
    row.set_title(title);
    row.set_value(get(&model.borrow()) as f64);
    let m = model.clone();
    row.connect_value_notify(move |r| set(&mut m.borrow_mut(), r.value() as u32));
    group.add(&row);
    row
}

fn group(page: &adw::PreferencesPage, title: &str, description: &str) -> adw::PreferencesGroup {
    let g = adw::PreferencesGroup::builder().title(title).build();
    if !description.is_empty() {
        g.set_description(Some(description));
    }
    page.add(&g);
    g
}

fn idx<T: PartialEq>(all: &[T], v: &T) -> usize {
    all.iter().position(|x| x == v).unwrap_or(0)
}

fn rebuild_folders(
    model: &Model,
    group: &adw::PreferencesGroup,
    rows: &Rc<RefCell<Vec<adw::ActionRow>>>,
) {
    for r in rows.borrow_mut().drain(..) {
        group.remove(&r);
    }
    let folders = model.borrow().resources.folders.clone();
    for (i, f) in folders.iter().enumerate() {
        let title = if f.name.is_empty() {
            f.path.as_str()
        } else {
            f.name.as_str()
        };
        let row = adw::ActionRow::builder()
            .title(title)
            .subtitle(&f.path)
            .build();
        let del = gtk::Button::builder()
            .icon_name("user-trash-symbolic")
            .valign(gtk::Align::Center)
            .tooltip_text("Remove")
            .build();
        del.add_css_class("flat");
        let (m, g, r) = (model.clone(), group.clone(), rows.clone());
        del.connect_clicked(move |_| {
            m.borrow_mut().resources.folders.remove(i);
            rebuild_folders(&m, &g, &r);
        });
        row.add_suffix(&del);
        group.add(&row);
        rows.borrow_mut().push(row);
    }
}

pub fn open(
    parent: &impl IsA<gtk::Widget>,
    profile: Profile,
    is_new: bool,
    groups: Vec<String>,
    on_save: impl Fn(EditorResult) + 'static,
) {
    let model: Model = Rc::new(RefCell::new(profile));
    let password: Rc<RefCell<Option<String>>> = Rc::default();
    let gw_password: Rc<RefCell<Option<String>>> = Rc::default();

    let stack = adw::ViewStack::new();

    // ------------------------------------------------------------ General
    let page = adw::PreferencesPage::new();
    let g = group(&page, "Computer", "");
    let name = entry(&g, &model, "Name", |p| p.name.clone(), |p, v| p.name = v);
    let host = entry(
        &g,
        &model,
        "Host",
        |p| p.host.clone(),
        |p, v| {
            // Accept host:port pasted into the host field.
            let (h, port) = rdpfile::split_host_port(&v);
            p.host = h;
            if let Some(port) = port {
                p.port = port;
            }
        },
    );
    spin(
        &g,
        &model,
        "Port",
        1.0,
        65535.0,
        |p| p.port as u32,
        |p, v| p.port = v as u16,
    );
    let group_row = entry(
        &g,
        &model,
        "Group",
        |p| p.group.clone(),
        |p, v| p.group = v.trim_matches('/').to_string(),
    );
    if !groups.is_empty() {
        group_row.set_tooltip_text(Some(&format!("Existing groups: {}", groups.join(", "))));
    }

    let g = group(
        &page,
        "Credentials",
        "Leave the password empty to be asked when connecting",
    );
    entry(
        &g,
        &model,
        "User name",
        |p| p.username.clone(),
        |p, v| p.username = v,
    );
    entry(
        &g,
        &model,
        "Domain",
        |p| p.domain.clone(),
        |p, v| p.domain = v,
    );
    let pw = adw::PasswordEntryRow::builder().title("Password").build();
    if model.borrow().save_password {
        pw.set_title("Password (stored in keyring — type to replace)");
    }
    let pwc = password.clone();
    pw.connect_changed(move |r| {
        pwc.replace(Some(r.text().to_string()));
    });
    g.add(&pw);
    switch(
        &g,
        &model,
        "Remember password",
        "Stored in the system keyring",
        |p| p.save_password,
        |p, v| p.save_password = v,
    );

    let g = group(&page, "Organize", "");
    switch(
        &g,
        &model,
        "Favorite",
        "",
        |p| p.favorite,
        |p, v| p.favorite = v,
    );
    entry(
        &g,
        &model,
        "Tags",
        |p| p.tags.join(", "),
        |p, v| {
            p.tags = v
                .split(',')
                .map(|t| t.trim().to_string())
                .filter(|t| !t.is_empty())
                .collect()
        },
    );
    entry(&g, &model, "Notes", |p| p.notes.clone(), |p, v| p.notes = v);
    stack.add_titled_with_icon(&page, Some("general"), "General", "computer-symbolic");

    // ------------------------------------------------------------ Display
    let page = adw::PreferencesPage::new();
    let g = group(&page, "Window", "");
    combo(
        &g,
        &model,
        "Start in",
        &["Window", "Fullscreen"],
        |p| (p.display.mode == WindowMode::Fullscreen) as usize,
        |p, i| {
            p.display.mode = if i == 1 {
                WindowMode::Fullscreen
            } else {
                WindowMode::Window
            }
        },
    );
    let res_options = [
        "Match window (dynamic)",
        "1280 × 720",
        "1366 × 768",
        "1600 × 900",
        "1920 × 1080",
        "2560 × 1440",
        "3840 × 2160",
        "Custom",
    ];
    let presets = [
        (0, 0),
        (1280, 720),
        (1366, 768),
        (1600, 900),
        (1920, 1080),
        (2560, 1440),
        (3840, 2160),
    ];
    let current_res = match model.borrow().display.resolution {
        Resolution::Dynamic => 0,
        Resolution::Fixed { width, height } => presets
            .iter()
            .position(|p| *p == (width, height))
            .unwrap_or(7),
    };
    let res_row = adw::ComboRow::builder()
        .title("Resolution")
        .model(&gtk::StringList::new(&res_options))
        .selected(current_res as u32)
        .build();
    g.add(&res_row);
    let (cw, ch) = match model.borrow().display.resolution {
        Resolution::Fixed { width, height } => (width, height),
        Resolution::Dynamic => (1920, 1080),
    };
    let w_row = adw::SpinRow::with_range(200.0, 8192.0, 2.0);
    w_row.set_title("Width");
    w_row.set_value(cw as f64);
    let h_row = adw::SpinRow::with_range(200.0, 8192.0, 1.0);
    h_row.set_title("Height");
    h_row.set_value(ch as f64);
    g.add(&w_row);
    g.add(&h_row);
    let apply_res = {
        let m = model.clone();
        let (w_row, h_row) = (w_row.clone(), h_row.clone());
        move |sel: usize| {
            let custom = sel == 7;
            w_row.set_visible(custom);
            h_row.set_visible(custom);
            m.borrow_mut().display.resolution = match sel {
                0 => Resolution::Dynamic,
                7 => Resolution::Fixed {
                    width: w_row.value() as u32 & !1,
                    height: h_row.value() as u32,
                },
                i => Resolution::Fixed {
                    width: presets[i].0,
                    height: presets[i].1,
                },
            };
        }
    };
    apply_res(current_res);
    {
        let apply = apply_res.clone();
        res_row.connect_selected_notify(move |r| apply(r.selected() as usize));
        let (apply, rr) = (apply_res.clone(), res_row.clone());
        w_row.connect_value_notify(move |_| apply(rr.selected() as usize));
        let (apply, rr) = (apply_res.clone(), res_row.clone());
        h_row.connect_value_notify(move |_| apply(rr.selected() as usize));
    }
    let scales = [0u32, 100, 125, 150, 175, 200, 250, 300];
    combo(
        &g,
        &model,
        "Scale",
        &[
            "Automatic",
            "100 %",
            "125 %",
            "150 %",
            "175 %",
            "200 %",
            "250 %",
            "300 %",
        ],
        |p| idx(&scales, &p.display.scale),
        move |p, i| p.display.scale = scales[i],
    );
    let depths = [32u32, 24, 16, 15, 8];
    combo(
        &g,
        &model,
        "Color depth",
        &["32 bit", "24 bit", "16 bit", "15 bit", "8 bit"],
        |p| idx(&depths, &p.display.color_depth),
        move |p, i| p.display.color_depth = depths[i],
    );

    let g = group(
        &page,
        "Multiple Monitors",
        "Used when the session runs fullscreen",
    );
    switch(
        &g,
        &model,
        "Use multiple monitors",
        "",
        |p| p.display.multimon,
        |p, v| p.display.multimon = v,
    );
    if let Some(display) = gtk::gdk::Display::default() {
        for monitor in monitors::all_monitors(&display) {
            let Some(connector) = monitor.connector().map(|c| c.to_string()) else {
                continue;
            };
            let selected = model.borrow().display.monitors.contains(&connector);
            let row = adw::SwitchRow::builder()
                .title(monitors::describe(&monitor))
                .active(selected || model.borrow().display.monitors.is_empty())
                .build();
            let m = model.clone();
            let all: Vec<String> = monitors::all_monitors(&display)
                .iter()
                .filter_map(|m| m.connector().map(|c| c.to_string()))
                .collect();
            row.connect_active_notify(move |r| {
                let mut p = m.borrow_mut();
                if p.display.monitors.is_empty() {
                    p.display.monitors = all.clone();
                }
                p.display.monitors.retain(|c| c != &connector);
                if r.is_active() {
                    p.display.monitors.push(connector.clone());
                }
                if p.display.monitors.len() == all.len() {
                    p.display.monitors.clear();
                }
            });
            g.add(&row);
        }
    }
    stack.add_titled_with_icon(&page, Some("display"), "Display", "video-display-symbolic");

    // ----------------------------------------------------------- Resources
    let page = adw::PreferencesPage::new();
    let g = group(&page, "Clipboard", "");
    let dirs = [
        ClipboardDirection::Both,
        ClipboardDirection::ToRemote,
        ClipboardDirection::ToLocal,
        ClipboardDirection::Off,
    ];
    combo(
        &g,
        &model,
        "Clipboard",
        &["Both directions", "Only to remote", "Only to local", "Off"],
        |p| idx(&dirs, &p.resources.clipboard),
        move |p, i| p.resources.clipboard = dirs[i],
    );
    switch(
        &g,
        &model,
        "Copy files",
        "Copy and paste files through the clipboard",
        |p| p.resources.clipboard_files,
        |p, v| p.resources.clipboard_files = v,
    );

    let g = group(&page, "Audio", "");
    let audio = [AudioMode::Local, AudioMode::Remote, AudioMode::Off];
    combo(
        &g,
        &model,
        "Sound",
        &[
            "Play on this computer",
            "Leave on remote computer",
            "Do not play",
        ],
        |p| idx(&audio, &p.resources.audio),
        move |p, i| p.resources.audio = audio[i],
    );
    switch(
        &g,
        &model,
        "Microphone",
        "",
        |p| p.resources.microphone,
        |p, v| p.resources.microphone = v,
    );
    switch(
        &g,
        &model,
        "Webcam",
        "Camera redirection (Windows 11 / Server 2022+)",
        |p| p.resources.webcam,
        |p, v| p.resources.webcam = v,
    );

    let g = group(
        &page,
        "Drives",
        "Folders appear under This PC on the remote computer",
    );
    switch(
        &g,
        &model,
        "Home folder",
        "",
        |p| p.resources.home_folder,
        |p, v| p.resources.home_folder = v,
    );
    switch(
        &g,
        &model,
        "Removable drives",
        "Including drives plugged in later",
        |p| p.resources.removable_drives,
        |p, v| p.resources.removable_drives = v,
    );
    let folders_group = group(&page, "Shared Folders", "");
    let add_folder = gtk::Button::builder()
        .icon_name("list-add-symbolic")
        .tooltip_text("Add Folder")
        .valign(gtk::Align::Center)
        .build();
    add_folder.add_css_class("flat");
    folders_group.set_header_suffix(Some(&add_folder));
    let folder_rows: Rc<RefCell<Vec<adw::ActionRow>>> = Rc::default();
    let rebuild: Rc<dyn Fn()> = {
        let (m, fg, rows) = (model.clone(), folders_group.clone(), folder_rows.clone());
        Rc::new(move || rebuild_folders(&m, &fg, &rows))
    };
    rebuild();
    {
        let (m, rebuild) = (model.clone(), rebuild.clone());
        add_folder.connect_clicked(move |b| {
            let dialog = gtk::FileDialog::builder().title("Share Folder").build();
            let (m, rebuild) = (m.clone(), rebuild.clone());
            dialog.select_folder(
                b.root().and_downcast_ref::<gtk::Window>(),
                gio::Cancellable::NONE,
                move |res| {
                    if let Some(path) = res.ok().and_then(|f| f.path()) {
                        m.borrow_mut().resources.folders.push(SharedFolder {
                            name: String::new(),
                            path: path.to_string_lossy().into_owned(),
                        });
                        rebuild();
                    }
                },
            );
        });
    }

    let g = group(&page, "Devices", "");
    switch(
        &g,
        &model,
        "Printers",
        "",
        |p| p.resources.printers,
        |p, v| p.resources.printers = v,
    );
    switch(
        &g,
        &model,
        "Smart cards",
        "Also enables smart card sign-in",
        |p| p.resources.smartcards,
        |p, v| p.resources.smartcards = v,
    );
    switch(
        &g,
        &model,
        "All USB devices",
        "Requires access to /dev/bus/usb",
        |p| p.resources.usb_auto,
        |p, v| p.resources.usb_auto = v,
    );
    entry(
        &g,
        &model,
        "USB devices (vid:pid, comma separated)",
        |p| p.resources.usb_devices.join(", "),
        |p, v| {
            p.resources.usb_devices = v
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        },
    );
    switch(
        &g,
        &model,
        "Serial ports",
        "",
        |p| p.resources.serial_ports,
        |p, v| p.resources.serial_ports = v,
    );
    switch(
        &g,
        &model,
        "SSH agent",
        "Forward the local SSH agent",
        |p| p.resources.ssh_agent,
        |p, v| p.resources.ssh_agent = v,
    );
    stack.add_titled_with_icon(
        &page,
        Some("resources"),
        "Resources",
        "drive-harddisk-symbolic",
    );

    // ------------------------------------------------------------- Network
    let page = adw::PreferencesPage::new();
    let g = group(&page, "Remote Desktop Gateway", "");
    let usages = [
        GatewayUsage::Never,
        GatewayUsage::Always,
        GatewayUsage::Detect,
    ];
    combo(
        &g,
        &model,
        "Use gateway",
        &["Never", "Always", "When the host is not reachable"],
        |p| idx(&usages, &p.gateway.usage),
        move |p, i| p.gateway.usage = usages[i],
    );
    entry(
        &g,
        &model,
        "Gateway host",
        |p| p.gateway.host.clone(),
        |p, v| p.gateway.host = v.trim().to_string(),
    );
    spin(
        &g,
        &model,
        "Gateway port",
        1.0,
        65535.0,
        |p| p.gateway.port as u32,
        |p, v| p.gateway.port = v as u16,
    );
    let transports = [
        GatewayTransport::Auto,
        GatewayTransport::Http,
        GatewayTransport::Rpc,
        GatewayTransport::Arm,
    ];
    combo(
        &g,
        &model,
        "Transport",
        &["Automatic", "HTTP", "RPC", "Azure (ARM)"],
        |p| idx(&transports, &p.gateway.transport),
        move |p, i| p.gateway.transport = transports[i],
    );
    switch(
        &g,
        &model,
        "Websockets",
        "",
        |p| p.gateway.websockets,
        |p, v| p.gateway.websockets = v,
    );
    switch(
        &g,
        &model,
        "Use the same credentials",
        "",
        |p| p.gateway.same_credentials,
        |p, v| p.gateway.same_credentials = v,
    );
    entry(
        &g,
        &model,
        "Gateway user name",
        |p| p.gateway.username.clone(),
        |p, v| p.gateway.username = v,
    );
    entry(
        &g,
        &model,
        "Gateway domain",
        |p| p.gateway.domain.clone(),
        |p, v| p.gateway.domain = v,
    );
    let gpw = adw::PasswordEntryRow::builder()
        .title("Gateway password")
        .build();
    let gpc = gw_password.clone();
    gpw.connect_changed(move |r| {
        gpc.replace(Some(r.text().to_string()));
    });
    g.add(&gpw);
    switch(
        &g,
        &model,
        "Remember gateway password",
        "",
        |p| p.gateway.save_password,
        |p, v| p.gateway.save_password = v,
    );

    let g = group(
        &page,
        "SSH Tunnel",
        "Connect through an SSH server (key or agent authentication)",
    );
    switch(
        &g,
        &model,
        "Use SSH tunnel",
        "",
        |p| p.ssh.enabled,
        |p, v| p.ssh.enabled = v,
    );
    entry(
        &g,
        &model,
        "SSH server (user@host or alias)",
        |p| p.ssh.host.clone(),
        |p, v| p.ssh.host = v.trim().to_string(),
    );
    spin(
        &g,
        &model,
        "SSH port",
        1.0,
        65535.0,
        |p| p.ssh.port as u32,
        |p, v| p.ssh.port = v as u16,
    );
    entry(
        &g,
        &model,
        "Identity file",
        |p| p.ssh.identity_file.clone(),
        |p, v| p.ssh.identity_file = v,
    );
    entry(
        &g,
        &model,
        "Jump hosts (ProxyJump)",
        |p| p.ssh.jump.clone(),
        |p, v| p.ssh.jump = v,
    );

    let g = group(&page, "Wake-on-LAN", "");
    switch(
        &g,
        &model,
        "Wake the computer before connecting",
        "",
        |p| p.wol.enabled,
        |p, v| p.wol.enabled = v,
    );
    entry(
        &g,
        &model,
        "MAC address",
        |p| p.wol.mac.clone(),
        |p, v| p.wol.mac = v.trim().to_string(),
    );
    entry(
        &g,
        &model,
        "Broadcast address",
        |p| p.wol.broadcast.clone(),
        |p, v| p.wol.broadcast = v.trim().to_string(),
    );
    spin(
        &g,
        &model,
        "Wait for boot (seconds)",
        0.0,
        600.0,
        |p| p.wol.wait,
        |p, v| p.wol.wait = v,
    );

    let g = group(&page, "Performance", "");
    let nets = [
        NetworkProfile::Auto,
        NetworkProfile::Lan,
        NetworkProfile::BroadbandHigh,
        NetworkProfile::Broadband,
        NetworkProfile::BroadbandLow,
        NetworkProfile::Wan,
        NetworkProfile::Modem,
    ];
    combo(
        &g,
        &model,
        "Connection",
        &[
            "Detect automatically",
            "LAN",
            "High-speed broadband",
            "Broadband",
            "Low-speed broadband",
            "WAN",
            "Modem",
        ],
        |p| idx(&nets, &p.performance.network),
        move |p, i| p.performance.network = nets[i],
    );
    let codecs = [
        Codec::Auto,
        Codec::Avc444,
        Codec::Avc420,
        Codec::RemoteFx,
        Codec::Progressive,
        Codec::Legacy,
    ];
    combo(
        &g,
        &model,
        "Graphics",
        &[
            "Automatic",
            "H.264 AVC444 (best quality)",
            "H.264 AVC420",
            "RemoteFX",
            "Progressive",
            "Legacy (no GFX)",
        ],
        |p| idx(&codecs, &p.performance.codec),
        move |p, i| p.performance.codec = codecs[i],
    );
    switch(
        &g,
        &model,
        "Desktop background",
        "",
        |p| p.performance.wallpaper,
        |p, v| p.performance.wallpaper = v,
    );
    switch(
        &g,
        &model,
        "Visual styles",
        "",
        |p| p.performance.themes,
        |p, v| p.performance.themes = v,
    );
    switch(
        &g,
        &model,
        "Font smoothing",
        "",
        |p| p.performance.font_smoothing,
        |p, v| p.performance.font_smoothing = v,
    );
    switch(
        &g,
        &model,
        "Desktop composition",
        "",
        |p| p.performance.desktop_composition,
        |p, v| p.performance.desktop_composition = v,
    );
    switch(
        &g,
        &model,
        "Show window contents while dragging",
        "",
        |p| p.performance.window_drag,
        |p, v| p.performance.window_drag = v,
    );
    switch(
        &g,
        &model,
        "Menu animations",
        "",
        |p| p.performance.menu_animations,
        |p, v| p.performance.menu_animations = v,
    );
    switch(
        &g,
        &model,
        "Compression",
        "",
        |p| p.performance.compression,
        |p, v| p.performance.compression = v,
    );
    switch(
        &g,
        &model,
        "UDP transport",
        "",
        |p| p.performance.udp,
        |p, v| p.performance.udp = v,
    );
    switch(
        &g,
        &model,
        "Reconnect automatically",
        "",
        |p| p.performance.auto_reconnect,
        |p, v| p.performance.auto_reconnect = v,
    );
    stack.add_titled_with_icon(&page, Some("network"), "Network", "network-wired-symbolic");

    // ------------------------------------------------------------ Advanced
    let page = adw::PreferencesPage::new();
    let g = group(&page, "Security", "");
    let protos = [
        SecurityProtocol::Auto,
        SecurityProtocol::Nla,
        SecurityProtocol::Tls,
        SecurityProtocol::Rdp,
        SecurityProtocol::Ext,
        SecurityProtocol::Aad,
    ];
    combo(
        &g,
        &model,
        "Protocol",
        &[
            "Negotiate",
            "NLA (CredSSP)",
            "TLS",
            "RDP",
            "Extended NLA",
            "Microsoft Entra ID",
        ],
        |p| idx(&protos, &p.security.protocol),
        move |p, i| p.security.protocol = protos[i],
    );
    switch(
        &g,
        &model,
        "Administrator session",
        "Connect to the console session",
        |p| p.security.admin,
        |p, v| p.security.admin = v,
    );
    switch(
        &g,
        &model,
        "Restricted admin mode",
        "Do not send credentials to the remote computer",
        |p| p.security.restricted_admin,
        |p, v| p.security.restricted_admin = v,
    );
    switch(
        &g,
        &model,
        "Smart card sign-in",
        "",
        |p| p.security.smartcard_logon,
        |p, v| p.security.smartcard_logon = v,
    );
    switch(
        &g,
        &model,
        "Ignore certificate errors",
        "Insecure: accepts any server identity",
        |p| p.security.ignore_certificate,
        |p, v| p.security.ignore_certificate = v,
    );
    entry(
        &g,
        &model,
        "Kerberos KDC proxy URL",
        |p| p.security.kdc_url.clone(),
        |p, v| p.security.kdc_url = v,
    );
    entry(
        &g,
        &model,
        "Load balance info",
        |p| p.security.load_balance_info.clone(),
        |p, v| p.security.load_balance_info = v,
    );

    let g = group(
        &page,
        "RemoteApp",
        "Run a single application instead of a full desktop",
    );
    switch(
        &g,
        &model,
        "RemoteApp mode",
        "",
        |p| p.remote_app.enabled,
        |p, v| p.remote_app.enabled = v,
    );
    entry(
        &g,
        &model,
        "Program (path or ||alias)",
        |p| p.remote_app.program.clone(),
        |p, v| p.remote_app.program = v,
    );
    entry(
        &g,
        &model,
        "Display name",
        |p| p.remote_app.name.clone(),
        |p, v| p.remote_app.name = v,
    );
    entry(
        &g,
        &model,
        "Arguments",
        |p| p.remote_app.args.clone(),
        |p, v| p.remote_app.args = v,
    );
    entry(
        &g,
        &model,
        "Working directory",
        |p| p.remote_app.workdir.clone(),
        |p, v| p.remote_app.workdir = v,
    );

    let g = group(&page, "Input", "");
    switch(
        &g,
        &model,
        "Capture system shortcuts",
        "Send Super, Alt+Tab and similar keys to the remote computer (Ctrl+Alt+Home toggles)",
        |p| p.input.grab_keyboard,
        |p, v| p.input.grab_keyboard = v,
    );
    switch(
        &g,
        &model,
        "Relative mouse",
        "For games and 3D applications",
        |p| p.input.relative_mouse,
        |p, v| p.input.relative_mouse = v,
    );
    entry(
        &g,
        &model,
        "Keyboard layout id (e.g. 0x409)",
        |p| p.input.keyboard_layout.clone(),
        |p, v| p.input.keyboard_layout = v.trim().to_string(),
    );

    let g = group(&page, "Expert", "");
    entry(
        &g,
        &model,
        "Client name",
        |p| p.advanced.client_hostname.clone(),
        |p, v| p.advanced.client_hostname = v,
    );
    entry(
        &g,
        &model,
        "Time zone",
        |p| p.advanced.timezone.clone(),
        |p, v| p.advanced.timezone = v,
    );
    entry(
        &g,
        &model,
        "Proxy (http:// or socks5://)",
        |p| p.advanced.proxy.clone(),
        |p, v| p.advanced.proxy = v,
    );
    entry(
        &g,
        &model,
        "Preconnection blob",
        |p| p.advanced.preconnection_blob.clone(),
        |p, v| p.advanced.preconnection_blob = v,
    );
    entry(
        &g,
        &model,
        "Extra FreeRDP arguments",
        |p| p.advanced.extra_args.join(" "),
        |p, v| {
            p.advanced.extra_args = glib::shell_parse_argv(&v)
                .map(|a| {
                    a.into_iter()
                        .map(|s| s.to_string_lossy().into_owned())
                        .collect()
                })
                .unwrap_or_default()
        },
    );
    stack.add_titled_with_icon(
        &page,
        Some("advanced"),
        "Advanced",
        "preferences-other-symbolic",
    );

    // -------------------------------------------------------------- frame
    let switcher = adw::ViewSwitcher::builder()
        .stack(&stack)
        .policy(adw::ViewSwitcherPolicy::Wide)
        .build();
    let header = adw::HeaderBar::builder()
        .title_widget(&switcher)
        .show_end_title_buttons(false)
        .show_start_title_buttons(false)
        .build();
    let cancel = gtk::Button::with_mnemonic("_Cancel");
    let save = gtk::Button::with_mnemonic(if is_new { "_Add" } else { "_Save" });
    save.add_css_class("suggested-action");
    header.pack_start(&cancel);
    header.pack_end(&save);
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&stack));

    let dialog = adw::Dialog::builder()
        .title(if is_new {
            "New Connection"
        } else {
            "Edit Connection"
        })
        .content_width(880)
        .content_height(720)
        .child(&toolbar)
        .build();

    let validate = {
        let (m, save) = (model.clone(), save.clone());
        move || save.set_sensitive(!m.borrow().host.trim().is_empty())
    };
    validate();
    {
        let v = validate.clone();
        host.connect_changed(move |_| v());
    }
    let _ = name;

    cancel.connect_clicked(glib::clone!(
        #[weak]
        dialog,
        move |_| {
            dialog.close();
        }
    ));
    save.connect_clicked(glib::clone!(
        #[weak]
        dialog,
        move |_| {
            let profile = model.borrow().clone();
            on_save(EditorResult {
                profile,
                password: password.borrow().clone().filter(|p| !p.is_empty()),
                gateway_password: gw_password.borrow().clone().filter(|p| !p.is_empty()),
            });
            dialog.close();
        }
    ));
    dialog.present(Some(parent));
}
