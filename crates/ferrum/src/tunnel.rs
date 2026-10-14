//! SSH port forwarding through the system `ssh` binary.
//!
//! Using OpenSSH gives ~/.ssh/config, agents, ProxyJump and hardware keys for
//! free. Authentication must be non-interactive (keys or agent).

use std::io::Read;
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use ferrum_profiles::SshTunnel;

pub struct Tunnel {
    child: Child,
    pub local_port: u16,
}

impl Tunnel {
    /// Forward a free local port to `target:port` and wait until it is usable.
    pub fn open(cfg: &SshTunnel, target: &str, port: u16) -> Result<Tunnel> {
        if cfg.host.trim().is_empty() {
            bail!("SSH tunnel host is empty");
        }
        let local_port = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?
            .local_addr()?
            .port();
        let target = if target.contains(':') {
            format!("[{target}]")
        } else {
            target.to_string()
        };

        let mut cmd = Command::new("ssh");
        cmd.args(["-N", "-T"])
            .args(["-o", "ExitOnForwardFailure=yes"])
            .args(["-o", "BatchMode=yes"])
            .args(["-o", "ServerAliveInterval=15"])
            .args(["-L", &format!("127.0.0.1:{local_port}:{target}:{port}")]);
        if cfg.port != 0 && cfg.port != 22 {
            cmd.args(["-p", &cfg.port.to_string()]);
        }
        if !cfg.identity_file.is_empty() {
            cmd.args(["-i", &cfg.identity_file]);
        }
        if !cfg.jump.is_empty() {
            cmd.args(["-J", &cfg.jump]);
        }
        cmd.arg(cfg.host.trim())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());

        let mut child = cmd
            .spawn()
            .context("failed to run ssh (is OpenSSH installed?)")?;
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            if let Some(status) = child.try_wait()? {
                let mut err = String::new();
                if let Some(mut stderr) = child.stderr.take() {
                    let _ = stderr.read_to_string(&mut err);
                }
                bail!("ssh exited ({status}): {}", err.trim());
            }
            if TcpStream::connect_timeout(
                &(Ipv4Addr::LOCALHOST, local_port).into(),
                Duration::from_millis(200),
            )
            .is_ok()
            {
                return Ok(Tunnel { child, local_port });
            }
            if Instant::now() > deadline {
                let _ = child.kill();
                bail!("timed out waiting for the SSH tunnel");
            }
            std::thread::sleep(Duration::from_millis(150));
        }
    }
}

impl Drop for Tunnel {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
