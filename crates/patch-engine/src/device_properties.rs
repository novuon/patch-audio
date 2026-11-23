//! Repair null sinks retained by pipewire-pulse across Patch upgrades.
//! Run on the link keeper, never the audio callback. Reload only modules
//! whose actual label is wrong, restoring endpoint state and stream targets.

use serde_json::Value;

fn inventory(
    run: &mut impl FnMut(&[&str]) -> Option<String>,
    kind: &str,
) -> Result<Vec<Value>, String> {
    if kind == "modules" {
        // pactl's long JSON module listing omits indexes on current Pulse
        // clients. The short listing retains the IDs owner_module refers to.
        let text = run(&["pactl", "list", "short", "modules"]).ok_or("could not list modules")?;
        let mut modules = Vec::new();
        for line in text.lines() {
            let mut fields = line.split('\t');
            let index = fields.next().unwrap_or_default();
            // Native modules have n/a indexes and multiline arguments. Only
            // parse actual null-sink records, not their continuation lines.
            if fields.next() != Some("module-null-sink") {
                continue;
            }
            let name = "module-null-sink";
            let index = index
                .parse::<u64>()
                .map_err(|_| "null sink module without index")?;
            let argument = fields.next().ok_or("module without arguments")?;
            modules.push(serde_json::json!({"index": index, "name": name, "argument": argument}));
        }
        return Ok(modules);
    }
    let text = run(&["pactl", "-f", "json", "list", kind])
        .ok_or_else(|| format!("could not list {kind}"))?;
    serde_json::from_str(&text).map_err(|e| format!("invalid {kind}: {e}"))
}

/// Decode the outer module-argument layer, including the legacy arguments
/// that quoted only the nested description. Never interpret these as shell.
fn arguments(text: &str) -> Option<Vec<String>> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut quote = None;
    let mut escaped = false;
    for c in text.chars() {
        if escaped {
            word.push(c);
            escaped = false;
        } else if c == '\\' {
            escaped = true;
        } else if Some(c) == quote {
            quote = None;
        } else if quote.is_none() && matches!(c, '\'' | '"') {
            quote = Some(c);
        } else if quote.is_none() && c.is_whitespace() {
            if !word.is_empty() {
                words.push(std::mem::take(&mut word));
            }
        } else {
            word.push(c);
        }
    }
    if quote.is_some() || escaped {
        return None;
    }
    if !word.is_empty() {
        words.push(word);
    }
    Some(words)
}

fn replacement(text: &str, name: &str, description: &str) -> Option<Vec<String>> {
    let words = arguments(text)?;
    let value = |key: &str| words.iter().find_map(|w| w.strip_prefix(key));
    if value("sink_name=") != Some(name) {
        return None;
    }
    let passive = matches!(
        name,
        "patch.program" | "patch.input" | "patch.aux1" | "patch.aux2"
    );
    let mut result = Vec::new();
    for word in &words {
        let Some((key, value)) = word.split_once('=') else {
            // Unquoted legacy labels leave bare words (System Sounds).
            continue;
        };
        if matches!(key, "sink_properties" | "node.passive" | "device.class") {
            continue;
        }
        // These are all the module options Patch has ever supplied. Do not
        // silently discard an unknown customization when replacing a module.
        if !matches!(
            key,
            "sink_name" | "format" | "rate" | "channels" | "channel_map" | "media.class"
        ) {
            return None;
        }
        if value
            .chars()
            .any(|c| c.is_whitespace() || matches!(c, '\\' | '\'' | '"'))
        {
            return None;
        }
        result.push(word.clone());
    }
    result.push(patch_core::pulse::sink_properties(
        description,
        passive,
        name == "patch",
    ));
    Some(result)
}

fn checked(
    run: &mut impl FnMut(&[&str]) -> Option<String>,
    args: &[&str],
) -> Result<String, String> {
    run(args).ok_or_else(|| format!("failed: {}", args.join(" ")))
}

fn restore_level(
    run: &mut impl FnMut(&[&str]) -> Option<String>,
    kind: &str,
    endpoint: &Value,
) -> Result<(), String> {
    let name = endpoint["name"].as_str().ok_or("endpoint without name")?;
    let volumes = endpoint["volume"]
        .as_object()
        .ok_or("endpoint without volume")?;
    let map = endpoint["channel_map"]
        .as_str()
        .ok_or("endpoint without channel map")?;
    let mut args = vec![
        "pactl".to_string(),
        format!("set-{kind}-volume"),
        name.to_string(),
    ];
    for channel in map.split(',').map(str::trim) {
        args.push(
            volumes[channel]["value"]
                .as_u64()
                .ok_or("channel without volume")?
                .to_string(),
        );
    }
    checked(run, &args.iter().map(String::as_str).collect::<Vec<_>>())?;
    checked(
        run,
        &[
            "pactl",
            &format!("set-{kind}-mute"),
            name,
            if endpoint["mute"] == true { "1" } else { "0" },
        ],
    )?;
    Ok(())
}

/// A successful scan is done once per engine. Failed discovery never means
/// "missing" and never unloads anything. A failed replacement reloads the
/// original arguments before restoring state; callers report any failure.
pub(crate) fn repair(
    run: &mut impl FnMut(&[&str]) -> Option<String>,
) -> Result<Option<bool>, String> {
    let modules = inventory(run, "modules")?;
    let sinks = inventory(run, "sinks")?;
    let sources = inventory(run, "sources")?;
    let mut repairs = Vec::new();
    for module in &modules {
        if module["name"] != "module-null-sink" {
            continue;
        }
        let Some(old) = module["argument"].as_str() else {
            continue;
        };
        let Some(words) = arguments(old) else {
            continue;
        };
        let Some(name) = words.iter().find_map(|w| w.strip_prefix("sink_name=")) else {
            continue;
        };
        let Some(label) = patch_core::pulse::description(name) else {
            continue;
        };
        let owned: Vec<_> = sinks
            .iter()
            .chain(&sources)
            .filter(|e| e["owner_module"] == module["index"])
            .cloned()
            .collect();
        let passive = matches!(
            name,
            "patch.program" | "patch.input" | "patch.aux1" | "patch.aux2"
        );
        if !owned.iter().any(|e| {
            e["name"] == name
                && (e["description"] != label
                    || (passive
                        && e["properties"]["node.passive"] != "true"
                        && e["properties"]["node.passive"] != true)
                    || (name == "patch" && e["properties"]["device.class"] != "sound"))
        }) {
            continue;
        }
        let args = replacement(old, name, &label)
            .ok_or_else(|| format!("cannot safely repair arguments for {name}"))?;
        repairs.push((module.clone(), args, owned));
    }
    if repairs.is_empty() {
        return Ok(Some(false));
    }
    let inputs = inventory(run, "sink-inputs")?;
    let outputs = inventory(run, "source-outputs")?;
    // Never let unloading a live sink move audible streams to a full-volume
    // hardware fallback. Keep the migration pending until playback/capture
    // rests. Passive Patch plumbing and corked streams can be restored below.
    let active = |stream: &Value| {
        stream["corked"] != true
            && stream["properties"]["node.passive"] != "true"
            && stream["properties"]["node.passive"] != true
    };
    if inputs.iter().any(active) || outputs.iter().any(active) {
        return Ok(None);
    }
    let default_sink = checked(run, &["pactl", "get-default-sink"])?;
    let default_source = checked(run, &["pactl", "get-default-source"])?;
    // Validate state before any module is removed.
    for (_, _, endpoints) in &repairs {
        for endpoint in endpoints {
            let map = endpoint["channel_map"]
                .as_str()
                .ok_or("endpoint without channel map")?;
            for channel in map.split(',').map(str::trim) {
                endpoint["volume"][channel]["value"]
                    .as_u64()
                    .ok_or("endpoint without channel volume")?;
            }
        }
    }
    let mut errors = Vec::new();
    for (module, args, endpoints) in repairs {
        let id = module["index"]
            .as_u64()
            .ok_or("module without index")?
            .to_string();
        checked(run, &["pactl", "unload-module", &id])?;
        let mut load = vec!["pactl", "load-module", "module-null-sink"];
        load.extend(args.iter().map(String::as_str));
        if let Err(e) = checked(run, &load) {
            errors.push(e);
            checked(
                run,
                &[
                    "pactl",
                    "load-module",
                    "module-null-sink",
                    module["argument"].as_str().unwrap(),
                ],
            )?;
        }
        for endpoint in &endpoints {
            let name = endpoint["name"].as_str().ok_or("endpoint without name")?;
            let is_sink = sinks
                .iter()
                .any(|s| s["index"] == endpoint["index"] && s["name"] == name);
            let (kind, streams, target, move_op) = if is_sink {
                ("sink", &inputs, "sink", "move-sink-input")
            } else {
                ("source", &outputs, "source", "move-source-output")
            };
            if let Err(e) = restore_level(run, kind, endpoint) {
                errors.push(e);
            }
            // A short event stream may have ended during the reload. Only
            // move streams still present, never resurrect a finished stream.
            let live = match inventory(
                run,
                if is_sink {
                    "sink-inputs"
                } else {
                    "source-outputs"
                },
            ) {
                Ok(live) => live,
                Err(e) => {
                    errors.push(e);
                    streams.clone()
                }
            };
            for stream in streams.iter().filter(|s| s[target] == endpoint["index"]) {
                if !live.iter().any(|s| s["index"] == stream["index"]) {
                    continue;
                }
                if let Some(index) = stream["index"].as_u64()
                    && let Err(e) = checked(run, &["pactl", move_op, &index.to_string(), name])
                {
                    errors.push(e);
                }
            }
            let default = if is_sink {
                &default_sink
            } else {
                &default_source
            };
            if default.trim() == name
                && let Err(e) = checked(run, &["pactl", &format!("set-default-{kind}"), name])
            {
                errors.push(e);
            }
        }
    }
    if errors.is_empty() {
        Ok(Some(true))
    } else {
        Err(errors.join("; "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::os::unix::fs::DirBuilderExt;
    use std::path::PathBuf;
    use std::process::{Child, Command, Stdio};
    use std::time::{Duration, Instant};

    struct Server {
        root: PathBuf,
        children: Vec<Child>,
    }
    impl Server {
        fn command(&self, binary: &str) -> Command {
            let mut command = Command::new(binary);
            command
                .env("XDG_RUNTIME_DIR", self.root.join("run"))
                .env("PIPEWIRE_RUNTIME_DIR", self.root.join("run"))
                .env(
                    "PULSE_SERVER",
                    format!("unix:{}/run/pulse/native", self.root.display()),
                )
                .env("HOME", self.root.join("home"))
                .env("XDG_CONFIG_HOME", self.root.join("config"))
                .env("XDG_STATE_HOME", self.root.join("state"))
                .env("XDG_CACHE_HOME", self.root.join("cache"))
                .env("XDG_DATA_HOME", self.root.join("data"))
                .env(
                    "DBUS_SESSION_BUS_ADDRESS",
                    format!("unix:path={}/no-bus", self.root.display()),
                )
                .env_remove("PIPEWIRE_REMOTE")
                .env_remove("PIPEWIRE_CONFIG_DIR")
                .env_remove("PULSE_RUNTIME_PATH");
            command
        }
        fn run(&self, args: &[&str]) -> Option<String> {
            let result = self.command(args[0]).args(&args[1..]).output().ok()?;
            if !result.status.success() {
                eprintln!(
                    "private server command {args:?}: {}",
                    String::from_utf8_lossy(&result.stderr)
                );
            }
            result
                .status
                .success()
                .then(|| String::from_utf8_lossy(&result.stdout).into_owned())
        }
        fn spawn(&mut self, binary: &str, args: &[&str]) {
            let child = self
                .command(binary)
                .args(args)
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap();
            self.children.push(child);
        }
        fn new() -> Self {
            let root = std::env::temp_dir()
                .join(format!("patch-device-properties-{}", std::process::id()));
            std::fs::create_dir(&root).unwrap();
            std::fs::DirBuilder::new()
                .mode(0o700)
                .create(root.join("run"))
                .unwrap();
            let mut server = Self {
                root,
                children: Vec::new(),
            };
            server.spawn("pipewire", &[]);
            wait(|| server.root.join("run/pipewire-0").exists());
            server.spawn("pipewire-pulse", &[]);
            wait(|| server.root.join("run/pulse/native").exists());
            let policy = server.root.join("config/wireplumber/wireplumber.conf.d");
            std::fs::create_dir_all(&policy).unwrap();
            std::fs::write(
                policy.join("90-test.conf"),
                r#"
wireplumber.profiles = {
  routing-test = {
    inherits = [ base ]
    metadata.sm-settings = required
    metadata.sm-objects = required
    policy.standard = required
    hardware.audio = disabled
    hardware.bluetooth = disabled
    hardware.video-capture = disabled
    monitor.alsa = disabled
    monitor.alsa-midi = disabled
    monitor.bluez = disabled
    monitor.bluez-midi = disabled
    monitor.v4l2 = disabled
    monitor.libcamera = disabled
    support.dbus = disabled
    support.portal-permissionstore = disabled
    script.client.access-portal = disabled
  }
}
"#,
            )
            .unwrap();
            server.spawn("wireplumber", &["--profile=routing-test"]);
            wait(|| {
                server
                    .run(&["pw-metadata", "-l"])
                    .is_some_and(|s| s.contains("default"))
            });
            server
        }
    }
    impl Drop for Server {
        fn drop(&mut self) {
            for child in self.children.iter_mut().rev() {
                let _ = child.kill();
                let _ = child.wait();
            }
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }
    fn wait(mut condition: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !condition() {
            assert!(Instant::now() < deadline, "private PipeWire did not settle");
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    fn endpoint(server: &Server, kind: &str, name: &str) -> Value {
        inventory(&mut |args| server.run(args), kind)
            .unwrap()
            .into_iter()
            .find(|s| s["name"] == name)
            .unwrap()
    }

    #[test]
    #[ignore = "requires PipeWire, pipewire-pulse, WirePlumber 0.5 and Pulse clients; run with --ignored"]
    fn private_server_repairs_legacy_labels_and_preserves_audio_state() {
        let mut server = Server::new();
        for (name, properties, class) in [
            (
                "patch",
                "sink_properties=device.description=Patch device.class=sound",
                "Audio/Sink",
            ),
            (
                "patch.system-sounds",
                "sink_properties=device.description=Patch System Sounds",
                "Audio/Sink",
            ),
            (
                "patch.input",
                "sink_properties=device.description=\"Patch Input\" node.passive=true",
                "Audio/Source/Virtual",
            ),
            (
                "unrelated",
                "sink_properties=device.description=Unrelated",
                "Audio/Sink",
            ),
        ] {
            server
                .run(&[
                    "pactl",
                    "load-module",
                    "module-null-sink",
                    &format!("sink_name={name}"),
                    properties,
                    &format!("media.class={class}"),
                    "format=float32le",
                    "rate=44100",
                    "channels=2",
                    "channel_map=front-left,front-right",
                ])
                .unwrap();
        }
        assert_eq!(
            endpoint(&server, "sinks", "patch.system-sounds")["description"],
            "Patch"
        );
        assert_eq!(
            endpoint(&server, "sources", "patch.input")["description"],
            "Patch"
        );
        server.run(&["pactl", "set-default-sink", "patch"]).unwrap();
        server
            .run(&["pactl", "set-default-source", "patch.input"])
            .unwrap();
        server
            .run(&["pactl", "set-sink-volume", "patch", "19000", "27000"])
            .unwrap();
        server
            .run(&["pactl", "set-sink-mute", "patch", "1"])
            .unwrap();
        server
            .run(&[
                "pactl",
                "set-source-volume",
                "patch.input",
                "13000",
                "22000",
            ])
            .unwrap();
        server
            .run(&["pactl", "set-source-mute", "patch.input", "1"])
            .unwrap();
        let sink = endpoint(&server, "sinks", "patch");
        let source = endpoint(&server, "sources", "patch.input");
        let unrelated = endpoint(&server, "sinks", "unrelated");
        server.spawn("pacat", &["--playback", "--raw", "--device=patch"]);
        server
            .children
            .last_mut()
            .unwrap()
            .stdin
            .as_mut()
            .unwrap()
            .write_all(&[0; 8192])
            .unwrap();
        server.spawn("parec", &["--raw", "--device=patch.input"]);
        wait(|| {
            inventory(&mut |args| server.run(args), "sink-inputs")
                .unwrap()
                .len()
                == 1
                && inventory(&mut |args| server.run(args), "source-outputs")
                    .unwrap()
                    .len()
                    == 1
        });
        assert_eq!(repair(&mut |args| server.run(args)).unwrap(), None);
        // The graph stays untouched while audio/capture streams are active.
        assert_eq!(endpoint(&server, "sinks", "patch")["index"], sink["index"]);
        for _ in 0..2 {
            let mut child = server.children.pop().unwrap();
            child.kill().unwrap();
            child.wait().unwrap();
        }
        wait(|| {
            inventory(&mut |args| server.run(args), "sink-inputs")
                .unwrap()
                .is_empty()
                && inventory(&mut |args| server.run(args), "source-outputs")
                    .unwrap()
                    .is_empty()
        });
        server.run(&["pactl", "load-module", "module-loopback", "source=patch.system-sounds.monitor", "sink=patch",
            "sink_input_properties=\"node.name=patch.system-sounds.loopback node.passive=true\"",
            "source_output_properties=node.passive=true"]).unwrap();
        wait(|| {
            inventory(&mut |args| server.run(args), "sink-inputs")
                .unwrap()
                .len()
                == 1
                && inventory(&mut |args| server.run(args), "source-outputs")
                    .unwrap()
                    .len()
                    == 1
        });
        assert_eq!(repair(&mut |args| server.run(args)).unwrap(), Some(true));
        let inputs = inventory(&mut |args| server.run(args), "sink-inputs").unwrap();
        let outputs = inventory(&mut |args| server.run(args), "source-outputs").unwrap();
        assert_eq!(inputs.len(), 1, "passive loopback playback lost");
        assert_eq!(outputs.len(), 1, "passive loopback capture lost");
        assert_eq!(
            inputs[0]["sink"],
            endpoint(&server, "sinks", "patch")["index"]
        );
        assert_eq!(
            outputs[0]["source"],
            endpoint(&server, "sources", "patch.system-sounds.monitor")["index"]
        );
        for (kind, name, old) in [("sinks", "patch", sink), ("sources", "patch.input", source)] {
            let new = endpoint(&server, kind, name);
            assert_eq!(
                new["description"],
                patch_core::pulse::description(name).unwrap()
            );
            for key in ["volume", "mute", "sample_specification", "channel_map"] {
                assert_eq!(new[key], old[key], "{name}: {key}");
            }
        }
        assert_eq!(
            endpoint(&server, "sinks", "patch.system-sounds")["description"],
            "Patch System Sounds"
        );
        assert_eq!(
            endpoint(&server, "sinks", "unrelated")["index"],
            unrelated["index"]
        );
        assert_eq!(
            server.run(&["pactl", "get-default-sink"]).unwrap().trim(),
            "patch"
        );
        assert_eq!(
            server.run(&["pactl", "get-default-source"]).unwrap().trim(),
            "patch.input"
        );
        let nodes: Vec<Value> = serde_json::from_str(&server.run(&["pw-dump"]).unwrap()).unwrap();
        let props = nodes
            .iter()
            .find_map(|n| {
                (n["info"]["props"]["node.name"] == "patch.input").then_some(&n["info"]["props"])
            })
            .unwrap();
        assert_eq!(props["node.passive"], true);
        let modules = inventory(&mut |args| server.run(args), "modules").unwrap();
        assert_eq!(repair(&mut |args| server.run(args)).unwrap(), Some(false));
        assert_eq!(
            inventory(&mut |args| server.run(args), "modules").unwrap(),
            modules
        );

        // All creation paths use the same encoder, including source nodes
        // whose passive flag used to be lost after the first property.
        for name in [
            "patch.program",
            "patch.aux1",
            "patch.aux2",
            "patch.deck-a",
            "patch.deck-b",
            "patch.deck-d",
            "patch.app-1",
            "patch.stream-2",
        ] {
            let passive = matches!(name, "patch.program" | "patch.aux1" | "patch.aux2");
            let label = patch_core::pulse::description(name).unwrap();
            server
                .run(&[
                    "pactl",
                    "load-module",
                    "module-null-sink",
                    &format!("sink_name={name}"),
                    &patch_core::pulse::sink_properties(&label, passive, false),
                    if passive {
                        "media.class=Audio/Source/Virtual"
                    } else {
                        "media.class=Audio/Sink"
                    },
                ])
                .unwrap();
            assert_eq!(
                endpoint(&server, if passive { "sources" } else { "sinks" }, name)["description"],
                label
            );
        }
        assert_eq!(repair(&mut |args| server.run(args)).unwrap(), Some(false));

        // A rejected corrected load must restore the original module/state.
        server
            .run(&[
                "pactl",
                "load-module",
                "module-null-sink",
                "sink_name=patch.deck-c",
                "sink_properties=device.description=Patch Deck C",
            ])
            .unwrap();
        server
            .run(&["pactl", "set-sink-volume", "patch.deck-c", "21000"])
            .unwrap();
        let original = endpoint(&server, "sinks", "patch.deck-c");
        let mut reject = true;
        assert!(
            repair(&mut |args| {
                if reject && args.starts_with(&["pactl", "load-module"]) {
                    reject = false;
                    None
                } else {
                    server.run(args)
                }
            })
            .is_err()
        );
        let restored = endpoint(&server, "sinks", "patch.deck-c");
        assert_eq!(restored["description"], "Patch");
        assert_eq!(restored["volume"], original["volume"]);
        assert_eq!(repair(&mut |args| server.run(args)).unwrap(), Some(true));

        // Both parsing layers must also preserve punctuation in labels.
        let label = "Patch \"guest's\" \\ input";
        server
            .run(&[
                "pactl",
                "load-module",
                "module-null-sink",
                "sink_name=escaped",
                &patch_core::pulse::sink_properties(label, true, false),
            ])
            .unwrap();
        assert_eq!(endpoint(&server, "sinks", "escaped")["description"], label);
    }

    #[test]
    fn failed_inventory_never_mutates_the_graph() {
        let mut calls = Vec::new();
        let result = repair(&mut |args| {
            calls.push(args.join(" "));
            None
        });
        assert!(result.is_err());
        assert_eq!(calls, ["pactl list short modules"]);
    }
}
