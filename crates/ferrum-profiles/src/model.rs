//! Connection profile model.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Profile {
    pub id: Uuid,
    pub name: String,
    /// Slash separated folder path, e.g. "Work/Datacenter". Empty = root.
    pub group: String,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub domain: String,
    /// Password lives in the Secret Service keyring, never in the file.
    pub save_password: bool,
    pub tags: Vec<String>,
    pub favorite: bool,
    pub notes: String,
    /// Unix timestamp of the last successful connection.
    pub last_connected: Option<i64>,

    pub display: Display,
    pub security: Security,
    pub gateway: Gateway,
    pub resources: Resources,
    pub performance: Performance,
    pub remote_app: RemoteApp,
    pub ssh: SshTunnel,
    pub wol: WakeOnLan,
    pub input: Input,
    pub advanced: Advanced,
}

impl Default for Profile {
    fn default() -> Self {
        Profile {
            id: Uuid::new_v4(),
            name: String::new(),
            group: String::new(),
            host: String::new(),
            port: 3389,
            username: String::new(),
            domain: String::new(),
            save_password: false,
            tags: Vec::new(),
            favorite: false,
            notes: String::new(),
            last_connected: None,
            display: Display::default(),
            security: Security::default(),
            gateway: Gateway::default(),
            resources: Resources::default(),
            performance: Performance::default(),
            remote_app: RemoteApp::default(),
            ssh: SshTunnel::default(),
            wol: WakeOnLan::default(),
            input: Input::default(),
            advanced: Advanced::default(),
        }
    }
}

impl Profile {
    pub fn new(name: impl Into<String>, host: impl Into<String>) -> Self {
        Profile {
            name: name.into(),
            host: host.into(),
            ..Default::default()
        }
    }

    /// Name to show in lists: the explicit name or `host[:port]`.
    pub fn display_name(&self) -> String {
        if !self.name.trim().is_empty() {
            self.name.clone()
        } else {
            self.address()
        }
    }

    /// `host` or `host:port` (IPv6 hosts are bracketed).
    pub fn address(&self) -> String {
        let host = if self.host.contains(':') && !self.host.starts_with('[') {
            format!("[{}]", self.host)
        } else {
            self.host.clone()
        };
        if self.port == 3389 || self.port == 0 {
            host
        } else {
            format!("{host}:{}", self.port)
        }
    }

    /// Duplicate with a fresh id.
    pub fn duplicate(&self) -> Profile {
        Profile {
            id: Uuid::new_v4(),
            name: format!("{} (copy)", self.display_name()),
            last_connected: None,
            ..self.clone()
        }
    }

    /// Case-insensitive match on name, host, user, group, tags and notes.
    pub fn matches(&self, query: &str) -> bool {
        let q = query.trim().to_lowercase();
        if q.is_empty() {
            return true;
        }
        [
            &self.name,
            &self.host,
            &self.username,
            &self.group,
            &self.notes,
        ]
        .iter()
        .any(|f| f.to_lowercase().contains(&q))
            || self.tags.iter().any(|t| t.to_lowercase().contains(&q))
    }
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum WindowMode {
    #[default]
    Window,
    Fullscreen,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case", tag = "kind")]
pub enum Resolution {
    /// Follow the window size (display control channel).
    #[default]
    Dynamic,
    /// Fixed size, scaled to fit the window.
    Fixed { width: u32, height: u32 },
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Display {
    pub mode: WindowMode,
    pub resolution: Resolution,
    /// Desktop scale in percent; 0 = follow the local monitor scale.
    pub scale: u32,
    /// Use all (or the selected) monitors in fullscreen.
    pub multimon: bool,
    /// Connector names (e.g. "DP-1"); empty = all monitors.
    pub monitors: Vec<String>,
    pub color_depth: u32,
}

impl Default for Display {
    fn default() -> Self {
        Display {
            mode: WindowMode::Window,
            resolution: Resolution::Dynamic,
            scale: 0,
            multimon: false,
            monitors: Vec::new(),
            color_depth: 32,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum SecurityProtocol {
    /// Negotiate the best protocol.
    #[default]
    Auto,
    Nla,
    Tls,
    Rdp,
    Ext,
    /// Azure AD / Entra ID.
    Aad,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Security {
    pub protocol: SecurityProtocol,
    /// Connect to the console/admin session.
    pub admin: bool,
    /// Restricted admin mode (no credentials sent to the server).
    pub restricted_admin: bool,
    /// Accept any certificate without asking. Dangerous.
    pub ignore_certificate: bool,
    /// Kerberos KDC URL (KDC proxy), optional.
    pub kdc_url: String,
    /// Log on with a smartcard certificate.
    pub smartcard_logon: bool,
    /// Load balance info / routing token (RD Connection Broker).
    pub load_balance_info: String,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum GatewayTransport {
    #[default]
    Auto,
    Http,
    Rpc,
    /// Azure Virtual Desktop resource manager.
    Arm,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum GatewayUsage {
    #[default]
    Never,
    Always,
    /// Use the gateway only when the host is not directly reachable.
    Detect,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Gateway {
    pub usage: GatewayUsage,
    pub host: String,
    pub port: u16,
    pub transport: GatewayTransport,
    /// Reuse the session credentials for the gateway.
    pub same_credentials: bool,
    pub username: String,
    pub domain: String,
    pub save_password: bool,
    pub websockets: bool,
}

impl Default for Gateway {
    fn default() -> Self {
        Gateway {
            usage: GatewayUsage::Never,
            host: String::new(),
            port: 443,
            transport: GatewayTransport::Auto,
            same_credentials: true,
            username: String::new(),
            domain: String::new(),
            save_password: false,
            websockets: true,
        }
    }
}

impl Gateway {
    pub fn enabled(&self) -> bool {
        self.usage != GatewayUsage::Never && !self.host.trim().is_empty()
    }
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum AudioMode {
    /// Play on this computer.
    #[default]
    Local,
    /// Leave on the remote computer.
    Remote,
    Off,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ClipboardDirection {
    #[default]
    Both,
    /// Only remote → local.
    ToLocal,
    /// Only local → remote.
    ToRemote,
    Off,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct SharedFolder {
    pub name: String,
    pub path: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Resources {
    pub clipboard: ClipboardDirection,
    pub clipboard_files: bool,
    pub audio: AudioMode,
    pub microphone: bool,
    pub folders: Vec<SharedFolder>,
    pub home_folder: bool,
    /// Redirect hot-plugged removable drives.
    pub removable_drives: bool,
    pub printers: bool,
    pub smartcards: bool,
    /// Redirect all USB devices (`auto`) or specific `vid:pid` ids.
    pub usb_auto: bool,
    pub usb_devices: Vec<String>,
    pub webcam: bool,
    pub serial_ports: bool,
    pub ssh_agent: bool,
}

impl Default for Resources {
    fn default() -> Self {
        Resources {
            clipboard: ClipboardDirection::Both,
            clipboard_files: true,
            audio: AudioMode::Local,
            microphone: false,
            folders: Vec::new(),
            home_folder: false,
            removable_drives: false,
            printers: false,
            smartcards: false,
            usb_auto: false,
            usb_devices: Vec::new(),
            webcam: false,
            serial_ports: false,
            ssh_agent: false,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum NetworkProfile {
    #[default]
    Auto,
    Lan,
    BroadbandHigh,
    Broadband,
    BroadbandLow,
    Wan,
    Modem,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum Codec {
    /// Graphics pipeline, let the server pick (H.264 when available).
    #[default]
    Auto,
    Avc444,
    Avc420,
    RemoteFx,
    Progressive,
    /// Legacy bitmap updates only.
    Legacy,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Performance {
    pub network: NetworkProfile,
    pub codec: Codec,
    pub wallpaper: bool,
    pub themes: bool,
    pub font_smoothing: bool,
    pub desktop_composition: bool,
    pub window_drag: bool,
    pub menu_animations: bool,
    pub compression: bool,
    pub auto_reconnect: bool,
    pub reconnect_retries: u32,
    /// UDP multitransport.
    pub udp: bool,
}

impl Default for Performance {
    fn default() -> Self {
        Performance {
            network: NetworkProfile::Auto,
            codec: Codec::Auto,
            wallpaper: true,
            themes: true,
            font_smoothing: true,
            desktop_composition: true,
            window_drag: true,
            menu_animations: false,
            compression: true,
            auto_reconnect: true,
            reconnect_retries: 20,
            udp: true,
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct RemoteApp {
    pub enabled: bool,
    /// Program path or `||alias`.
    pub program: String,
    pub name: String,
    pub args: String,
    pub workdir: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct SshTunnel {
    pub enabled: bool,
    /// `user@host` or a Host alias from ~/.ssh/config.
    pub host: String,
    pub port: u16,
    pub identity_file: String,
    /// ProxyJump spec.
    pub jump: String,
}

impl Default for SshTunnel {
    fn default() -> Self {
        SshTunnel {
            enabled: false,
            host: String::new(),
            port: 22,
            identity_file: String::new(),
            jump: String::new(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct WakeOnLan {
    pub enabled: bool,
    pub mac: String,
    pub broadcast: String,
    pub port: u16,
    /// Seconds to wait for the host to come up.
    pub wait: u32,
}

impl Default for WakeOnLan {
    fn default() -> Self {
        WakeOnLan {
            enabled: false,
            mac: String::new(),
            broadcast: "255.255.255.255".into(),
            port: 9,
            wait: 60,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Input {
    /// Keyboard layout id (e.g. "0x409"); empty = derive from local layout.
    pub keyboard_layout: String,
    /// Capture system shortcuts (Super, Alt+Tab...) while focused.
    pub grab_keyboard: bool,
    pub relative_mouse: bool,
    /// Swap left/right buttons.
    pub swap_buttons: bool,
}

impl Default for Input {
    fn default() -> Self {
        Input {
            keyboard_layout: String::new(),
            grab_keyboard: true,
            relative_mouse: false,
            swap_buttons: false,
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Advanced {
    pub client_hostname: String,
    pub timezone: String,
    pub proxy: String,
    pub preconnection_blob: String,
    /// Raw extra xfreerdp-style arguments, appended last.
    pub extra_args: Vec<String>,
}
