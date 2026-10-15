# Ferrum

A native RDP client for Linux built with GTK4/libadwaita. It runs on
Wayland, and the RDP protocol itself is handled by
[FreeRDP 3](https://www.freerdp.com/).

## Features

- **Sessions in tabs.** A tab can be dragged out into its own window.
  Fullscreen mode reveals the header bar when the pointer touches the top
  edge.
- **Display.** Dynamic resolution that follows the window size, HiDPI and
  fractional scaling, fit-to-window or original-size scaling, and
  fullscreen across multiple monitors.
- **Graphics.** Everything FreeRDP decodes: H.264 AVC444/AVC420 (with VAAPI),
  RemoteFX and progressive codecs.
- **Input.** Keys are sent as scancodes, so the remote keyboard layout
  applies. While the session has focus, system shortcuts (Super, Alt+Tab)
  are captured and sent to the remote computer. High-resolution wheel and
  touchpad scrolling work, and so do the back/forward mouse buttons.
- **Clipboard.** Text, HTML, images and **files** in both directions.
  Remote files are exposed through FUSE.
- **Redirection.** Audio and microphone (PulseAudio/PipeWire), shared
  folders, the home folder, hot-plugged drives, printers, smart cards, USB
  devices, a webcam, serial ports and the SSH agent.
- **RemoteApp.** Each remote window becomes a separate local window.
- **Security.** NLA/TLS/RDP, Kerberos and KDC proxy, Microsoft Entra ID,
  admin and restricted-admin modes, and smart card sign-in. The server
  certificate is shown for confirmation.
- **RD Gateway.** HTTP, RPC, websocket and Azure ARM transports, with
  separate gateway credentials.
- **Connection manager.** Groups, favorites, recent connections, search and
  quick connect (`user@host:port`). Passwords are stored in the Secret
  Service keyring.
- **Import and export.** `.rdp` files (mstsc format, UTF-16) can be
  imported and exported, and connections can be imported from Remmina.
- **Network.** An SSH tunnel through the system `ssh` (ProxyJump and agent
  supported) and Wake-on-LAN before connecting.
- **Automatic reconnection** when the connection drops.

## Build

```sh
sudo pacman -S --needed freerdp fuse3 gtk4 libadwaita   # Arch / Omarchy
./install.sh            # installs into ~/.local
```

## Usage

```sh
ferrum                              # connection manager
ferrum user@host:3390               # quick connect
ferrum -c "Office Workstation"      # saved connection by name
ferrum file.rdp                     # open an .rdp file
ferrum host --app '||notepad'       # RemoteApp
ferrum host -f --multimon           # fullscreen on all monitors
ferrum --new host                   # new connection prefilled with host
ferrum --list                       # list saved connections
ferrum host -- /gfx:AVC444 /log-level:DEBUG   # extra FreeRDP arguments
```

Shortcuts inside a session:

| Shortcut | Action |
|---|---|
| Ctrl+Alt+Enter | Toggle fullscreen |
| Ctrl+Alt+End | Send Ctrl+Alt+Del |
| Ctrl+Alt+Home | Capture/release system shortcuts |

Connections are stored in `~/.config/ferrum/connections.toml`.

### Hyprland / Omarchy

Omarchy makes windows slightly transparent. For a remote desktop this is
better turned off: add a rule to your Lua config in `~/.config/hypr/`,
written like the default rule for qemu:

```lua
o.window("dev.ferrum.Rdp", { tag = "-default-opacity", opacity = "1 1" })
```

## Architecture

```
crates/
  freerdp-sys/      C shim (shim/ferrum_shim.c) over libfreerdp3 + FFI declarations
  rdp-core/         safe Rust API: Session, Handler, FrameBuffer, keymap
  ferrum-profiles/  profile model, TOML store, FreeRDP args, .rdp/Remmina
  ferrum/           GTK4/libadwaita application
```

The C shim follows the official FreeRDP clients (Wayland/X11). It owns the
`rdpContext`, the GDI renderer and the channels (cliprdr, disp, rail), and
exposes a narrow API made of a callback table and input functions. Each
session runs on its own thread. Frames go into a shared `FrameBuffer`, and
the UI updates only the changed regions of the texture through
`GdkMemoryTextureBuilder`.

## Tests

```sh
cargo test --workspace
# headless smoke test against a server: saves a PNG of the remote screen
cargo run -p rdp-core --example connect -- /v:host /u:user /p:pass out.png 5
```

## License

GPL-3.0-or-later, see [LICENSE](LICENSE).
