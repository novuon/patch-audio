//! `omarchy-patch network …`: the Network Unit without its window.

use serde_json::{Value, json};

fn print(v: &Value) -> anyhow::Result<()> {
    if let Some(m) = v["message"].as_str() {
        println!("{m}");
    } else {
        println!("{}", serde_json::to_string_pretty(v)?);
    }
    Ok(())
}

fn act(req: Value) -> anyhow::Result<()> {
    let v = patch_network::ops::act(&req).map_err(anyhow::Error::msg)?;
    print(&v)
}

/// The value after `--flag`, if given.
fn flag<'a>(rest: &[&'a str], name: &str) -> Option<&'a str> {
    rest.iter().position(|a| *a == name).and_then(|i| rest.get(i + 1).copied())
}

pub fn run(args: &[&str]) -> anyhow::Result<()> {
    match args {
        [] | ["status"] => {
            println!("{}", serde_json::to_string_pretty(&patch_network::ops::status())?);
            Ok(())
        }
        // Listen for announcements a few seconds and list what is offered.
        ["scan", rest @ ..] => {
            let secs = rest.first().and_then(|s| s.parse().ok()).unwrap_or(6u64);
            let found = patch_network::discover::scan(secs);
            if let Some(e) = patch_network::discover::error() {
                anyhow::bail!("can't listen for announcements: {e}");
            }
            let offered: Vec<_> = found.iter().filter(|a| patch_network::ops::offered(a)).collect();
            if offered.is_empty() {
                println!("no streams announced in {secs} s");
                if let Some(h) = patch_network::net::primary()
                    && let patch_network::firewall::Firewall::Blocked { command } =
                        patch_network::firewall::check(&h.subnet())
                {
                    println!("the firewall drops announcements; to let them in from this network:\n  {command}");
                }
            }
            for a in offered {
                println!(
                    "rx-{}  {}  from {}  {}{}",
                    a.id,
                    a.name,
                    a.origin,
                    a.format_label(),
                    if a.supported() { "" } else { "  (not playable)" }
                );
            }
            Ok(())
        }
        ["send", feed, rest @ ..] => act(json!({
            "op": "send",
            "feed": feed,
            "proto": if rest.contains(&"--vban") { "vban" } else { "rtp" },
            "to": flag(rest, "--to").unwrap_or_default(),
            "name": flag(rest, "--name").unwrap_or_default(),
        })),
        ["listen", id] => {
            // A stream is known once its announcement was heard.
            let _ = patch_network::discover::scan(6);
            act(json!({"op": "listen", "id": id}))
        }
        [op @ ("start" | "stop" | "remove"), id] => act(json!({"op": op, "id": id})),
        ["stop-all"] => {
            let n = patch_network::ops::stop_all().map_err(anyhow::Error::msg)?;
            println!("stopped {n} links");
            Ok(())
        }
        ["vban", on @ ("on" | "off")] => act(json!({"op": "vban", "on": *on == "on"})),
        ["latency", ms] => {
            let ms: u64 = ms.trim_end_matches("ms").parse()?;
            act(json!({"op": "latency", "ms": ms}))
        }
        ["allow"] => act(json!({"op": "allow"})),
        ["restore"] => {
            let exe = std::env::current_exe()?;
            for e in patch_network::ops::restore(&exe) {
                eprintln!("{e}");
            }
            Ok(())
        }
        // The service a link runs as (started by Patch, not by hand).
        ["link"] => {
            let link = patch_network::link::from_env().map_err(anyhow::Error::msg)?;
            patch_network::link::supervise(link).map_err(anyhow::Error::msg)
        }
        _ => anyhow::bail!(
            "usage: omarchy-patch network [status] | scan [secs] | send <program|aux1|aux2> [--to <ip>] [--vban] [--name N] | listen <rx-id> | start|stop|remove <id> | stop-all | vban on|off | latency <20|50|100|250> | allow"
        ),
    }
}
