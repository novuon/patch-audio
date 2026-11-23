//! Browser surfaces; never starts, replaces, or installs the audio engine.
mod access;
mod engine;
mod http;
mod identity;

use std::sync::Arc;

fn main() -> anyhow::Result<()> {
    let mut control = false;
    let mut origin = None;
    let mut login = None;
    let mut socket = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--control" => control = true,
            "--tailscale-origin" => {
                origin = Some(
                    args.next()
                        .ok_or_else(|| anyhow::anyhow!("Missing HTTPS origin"))?,
                )
            }
            "--tailscale-user" => {
                login = Some(
                    args.next()
                        .ok_or_else(|| anyhow::anyhow!("Missing account login"))?,
                )
            }
            "--tailscale-socket" => {
                socket =
                    Some(std::path::PathBuf::from(args.next().ok_or_else(|| {
                        anyhow::anyhow!("Missing tailscaled socket path")
                    })?));
            }
            "--help" => {
                println!(
                    "patch-remote [--control] [--tailscale-origin https://host.ts.net:port --tailscale-user login] [--tailscale-socket path]\nBinds 127.0.0.1:8124. Phone access uses Tailscale Serve plus local whois. This computer's browser needs the pairing secret."
                );
                return Ok(());
            }
            _ => anyhow::bail!("Unknown argument: {arg}"),
        }
    }
    let pairing = access::load_or_create().map_err(anyhow::Error::msg)?;
    let access = match (origin, login) {
        (None, None) => access::Access::local(pairing).map_err(anyhow::Error::msg)?,
        (Some(origin), Some(login)) => {
            access::Access::tailscale(origin, login, pairing).map_err(anyhow::Error::msg)?
        }
        _ => anyhow::bail!("Specify both --tailscale-origin and --tailscale-user"),
    };
    let identity = identity::Live::resolve(socket);
    eprintln!(
        "patch-remote: pairing secret is {} (mode 0600). It is not sent in any response. Local browsers must present it; localhost alone is not a trust domain.",
        access::pairing_path()
            .map_err(anyhow::Error::msg)?
            .display()
    );
    if let Some(origin) = access.phone_origin() {
        match identity.probe() {
            Ok(()) => eprintln!(
                "patch-remote: phone allowlist for {origin} is the login from tailscaled whois at {}",
                identity.socket().display()
            ),
            Err(err) => eprintln!(
                "patch-remote: Tailscale local API is not reachable ({err}). Phone access is denied until whois works. This user must be able to open tailscaled.sock."
            ),
        }
    }
    let bridge = engine::Bridge::start(control)?;
    http::serve(bridge, access, Arc::new(identity))
}
