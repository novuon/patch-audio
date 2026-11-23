//! `omarchy-patch`: Patch, the Omarchy audio application.
//!
//! Named `omarchy-patch` rather than `patch` so it never shadows
//! `/usr/bin/patch`. One process is one Patch session; a second invocation
//! talks to the running session over its control socket.

use patch_core::AppId;
use patch_core::command::{Command, Origin};
use patch_core::ipc::Request;
use patch_host::{Launch, ipc};
use patch_surfaces::AudioApp;
use std::io::Write as _;
use std::path::PathBuf;

mod autoeq_cli;
mod bar;
mod library_cli;
mod licence_cli;
mod perf_cli;
mod player_cli;
mod mic_cli;
mod setup_cli;
mod dj_cli;
mod dj_ai_cli;
mod engine_cli;
mod media_cli;
mod modules_cli;
mod plugins_cli;
mod network_cli;

const USAGE: &str = "\
omarchy-patch — Patch, the Omarchy audio environment

open
  omarchy-patch                        the controller (Ctrl+K in any Patch window)
  omarchy-patch layout [<preset>]      your default, or: simple · basic-dj · ai-dj · performance-dj · dj-studio
                                       · podcast · live-code · pro (· all-units, for development),
                                       or one you saved
                                       (opens on the next empty workspace; see docs/layouts.md)
  omarchy-patch welcome                the short guide shown after installing
  omarchy-patch layout show|reset|close          the layout on screen: go to it, put its windows
                                       back as saved, close them (audio keeps playing)
  omarchy-patch layout save [name] | delete <id> save the windows as they are now (a built-in
                                       layout needs a new name) · delete a saved one
  omarchy-patch open <surface>...      a Unit's window: graph mixer inspector meters analyzer
                                       recorder player mic sequencer presets devices controllers deck-a deck-b
                                       dj-mixer library diagnostics dj pattern tidal plugins
                                       plugin plugin#N (a rack plugin's own window)
  omarchy-patch plugins status | installed | add <name> | remove|on|off <slot> | move <slot> up|down
                       | view <slot> patch|original | parameters <slot> | set <slot> <id> <0..1>
                       | install <download.zip|.tar.gz|.vst3>
                                       the VST3 rack on the master: on, off and turned live
  omarchy-patch close|toggle <surface> the newest window of it, or one: close meters#2
  omarchy-patch status | quit          the UI (quitting never stops audio)

route and mix
  omarchy-patch route|bypass <app>     play an app through the Patch bus, or straight out again
  omarchy-patch send <from> <to>       patch a stream, bus or master to an output
  omarchy-patch gain <strip> <dB> | mute|unmute <strip> | restore
  omarchy-patch record start [bus|output|<app>] [f32|s24] | record stop | pause | resume | status
  omarchy-patch mic profiles | profile <mic-N> <name> | save <mic-N> <name> | restore <mic-N>
  omarchy-patch mic calibrate <take> | compare <take> <profile.json> <processed.wav>
  omarchy-patch player load <file|URL> | play | pause | toggle | stop | status
  omarchy-patch player seek <0..100%> | volume <0..100> | loop on|off | quit
  omarchy-patch player add <file|URL>… | next | prev | queue [<n>|clear|shuffle] | speed <0.5..2> | repeat off|one|all | ab
  omarchy-patch player cart <pad> | cart <bank> <pad> | cart stop|fade [<pad>] | cart status
  omarchy-patch broadcast start <icecast|rtmp|file> <target> [--mount M] [--user U] [--password P]
                                       [--key K] [--picture performer|visualiser|none]
                                       [--codec opus|mp3|aac] [--bitrate <kbps>]
                                       [--source device:<node>] [--name N] | broadcast stop | status
  omarchy-patch network [status] | scan | send <program|aux1|aux2> [--to <ip>] [--vban] [--name N]
                                       | listen <rx-id> | start|stop|remove <id> | vban on|off
                                       | latency <20|50|100|250> | allow   audio to and from the local network

dj (works without any window: the Patch service runs it)
  omarchy-patch deck a|b|c|d load <name> | play | pause | toggle | cue | eject | sync | tempo <±%>
  omarchy-patch deck a|b loop <beats>|off | reloop | jump <±beats> | fx <effect>[+<effect>…] [beats] [depth] | fx off
      effects: echo reverb flanger trans crush roll brake pingpong phaser noise sweep tremolo pan drive
      a chain of up to three runs in series on one depth knob: deck a fx echo+sweep 1 0.6
  omarchy-patch xfader <0..1> | dj blend a b | dj cancel
  omarchy-patch dj queue add <file> | remove <n> | clear
  omarchy-patch dj automix on|off [build|hold|release] | dj next
  omarchy-patch dj set start [minutes] [journey|warmup|peak] [folder] | dj set stop   the AI DJ plays a set
  omarchy-patch dj say keep the energy up | stay around 126 | surprise me | next
  omarchy-patch dj ai suggest|prepare|run|auto|off [build|hold|release]
  omarchy-patch dj levels [loud|balanced|streaming]   how loud the decks play (−9, −11, −14 LUFS)
  omarchy-patch dj style [plain|balanced|adventurous]   how adventurous the AI's transitions are
  omarchy-patch dj style <effect-exits|accents|loops|stem-led|bass-replace|prefer-stems> on|off
  omarchy-patch dj tempo [follow|hold <bpm>|faster|slower|original]   where the AI takes the set's tempo
  omarchy-patch dj play-length [20..100]   about how much of each track plays, in percent (100 = all)
  omarchy-patch dj stems [auto|off|when-needed|ahead|everything]   which tracks get stems prepared by themselves
  omarchy-patch library                the sources, and how many tracks are ready, waiting or failed
  omarchy-patch library add <folder> [name] | remove <name> | rescan | failed
                                       a folder or USB stick in the library: found and analysed in the background
  omarchy-patch library collection list | create <name> <folder> | add <name> <file>...
  omarchy-patch analyze <file>...      tempo and grid, key (Camelot), loudness (cached)
  omarchy-patch dj package create|inspect|validate|add|migrate|relink …

live code (Tidal: TidalCycles, native; works without any window)
  omarchy-patch tidal eval '<code>' | eval -          run a block (d1 $ s \"bd*4\", hush, setcps 0.5 …);
                                       `-` reads stdin, so any editor can send the block under its cursor
  omarchy-patch tidal run <file> | hush | play | stop | status | tempo <bpm> | follow <auto|a|b|c|d>
  omarchy-patch tidal samples                          the sample folders (~/.local/share/omarchy-patch/samples/<name>/)
  omarchy-patch tidal sample add <name> <file> [from] [to]   cut a new sample of <name> from any audio file (seconds)
  omarchy-patch pattern status|play|stop|load <file> [patch|strudel]   the Pattern Unit (key t)

engine and machine
  omarchy-patch controllers status|reload                  MIDI & HID unit: devices and profiles
  omarchy-patch controllers import <mixxx.xml> [--write]     convert a Mixxx mapping to a Patch profile
  omarchy-patch engine start|status|upgrade|quit [--force]   the Patch service (gapless upgrade)
  omarchy-patch service install|uninstall|status             start it with the session (systemd --user)
  omarchy-patch setup [profile standard|dj|podcast|live_coding|full | <switch> on|off]   what Patch does here
                                       (system_audio dj start_with_session bar themes; Ctrl+K too)
  omarchy-patch modules [add|remove <id>… | status | profiles]   add or remove modules; big downloads run in the background
  omarchy-patch licence [activate <key> | deactivate]       activate a key for a module that needs one
  omarchy-patch perf [fair] [seconds] [baseline]             what Patch costs next to PipeWire (Performance Unit, SUPER+ALT+P);
                                                             fair: the service alone, windows closed for the test
  omarchy-patch bar install|uninstall                        Patch in the bar's audio dropdown (EQ, layouts)
  omarchy-patch bar health on|off                            a dot in the bar for Patch's audio buffers
  omarchy-patch health                                       buffer health: green, amber or red, and why
  omarchy-patch eq [master|apps|dj|mic-N] band <1-8> [on|off] [bell|lowshelf|highshelf|lowpass|highpass|notch] [Hz] [dB] [Q]
  omarchy-patch eq [master|apps|dj|mic-N] bypass on|off | reset   EQs: the program, apps, the DJ Booth, mics (EQ Unit, key k)
  omarchy-patch eq [master|apps|dj|mic-N] gain <dB>         input gain before the bands (±18; a correction's preamp)
  omarchy-patch eq [master|apps|dj|mic-N] preset <name> | save <name>   presets (patch-eq JSON)
  omarchy-patch eq app <name> [off]                          an application gets its own EQ (target app:<name>)
  omarchy-patch eq | eq presets                               every EQ at a glance · the presets
  omarchy-patch eq autoeq [search] <headphone> [--target T] [--no-load]   headphone correction from AutoEq
  omarchy-patch latency [measure [mic]|forget]               measured output latency
  omarchy-patch optimize [--quick] [--dry-run]               benchmark and pick DSP kernels
  omarchy-patch shots [dir] [surface]  render screenshots headlessly
  omarchy-patch hypr-rules             print Hyprland rules for a persistent install
  omarchy-patch help | --version";

const APP: AppId = AppId::Patch;

fn send(req: Request) -> Option<String> {
    ipc::send(APP, &req)
}

/// A broadcast started from the CLI: `broadcast start <kind> <target> [flags]`.
fn broadcast_config(
    kind: &str,
    target: &str,
    rest: &[&str],
) -> anyhow::Result<patch_broadcast::daemon::Config> {
    let mut v = serde_json::json!({
        "kind": kind,
        "target": target,
        "source": format!("device:{}", patch_engine::links::PROGRAM),
        "label": "Patch program",
        "codec": "opus",
        "bitrate": 128,
        "rate": 48_000,
        "channels": 2,
        "name": "Patch",
    });
    let mut i = 0;
    while i < rest.len() {
        let val = rest.get(i + 1).copied();
        let set = |v: &mut serde_json::Value, k: &str| {
            if let Some(x) = val {
                v[k] = serde_json::json!(x);
            }
        };
        match rest[i] {
            "--mount" => set(&mut v, "mount"),
            "--user" => set(&mut v, "user"),
            "--password" => set(&mut v, "password"),
            "--key" => set(&mut v, "key"),
            "--picture" => set(&mut v, "picture"),
            "--codec" => set(&mut v, "codec"),
            "--name" => set(&mut v, "name"),
            "--source" => set(&mut v, "source"),
            "--bitrate" => {
                if let Some(n) = val.and_then(|s| s.parse::<u64>().ok()) {
                    v["bitrate"] = n.into();
                }
            }
            "--rate" => {
                if let Some(n) = val.and_then(|s| s.parse::<u64>().ok()) {
                    v["rate"] = n.into();
                }
            }
            other => anyhow::bail!("unknown broadcast option {other}"),
        }
        i += 2;
    }
    patch_broadcast::daemon::config_from_json(&v).map_err(anyhow::Error::msg)
}

/// A request about the layout on screen: `ok`, `ok <id>` (saved as) or `err <why>`.
fn layout_request(req: Request) -> anyhow::Result<()> {
    match send(req).as_deref() {
        None => anyhow::bail!("Patch has no windows open"),
        Some("ok") => Ok(()),
        Some(reply) => match reply.strip_prefix("ok ") {
            Some(id) => {
                println!("saved as '{id}': omarchy-patch layout {id}");
                Ok(())
            }
            None => anyhow::bail!("{}", reply.strip_prefix("err ").unwrap_or(reply)),
        },
    }
}

#[cfg(all(feature = "jemalloc", feature = "dhat-heap"))]
compile_error!("DHAT profiling requires --no-default-features --features dhat-heap (glibc allocator)");

// All commands, including the engine daemon, enter through this binary.
// Unprefixed jemalloc also replaces malloc/free in linked native libraries.
#[cfg(all(feature = "jemalloc", not(feature = "dhat-heap")))]
#[global_allocator]
static ALLOC: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

#[cfg(feature = "dhat-heap")]
#[global_allocator]
static ALLOC: dhat::Alloc = dhat::Alloc;

/// `println!` panics when its reader is gone. With `| head` that is no fault:
/// end as a piped program does (128 + SIGPIPE), without a panic message or a
/// core dump. The main thread only, and a main thread's panic ends the process
/// anyway; anything else keeps the normal panic.
fn closed_stdout_is_no_crash() {
    let normal = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let main = std::thread::current().name() == Some("main");
        if main && info.payload_as_str().is_some_and(is_closed_stdout) {
            std::process::exit(141);
        }
        normal(info);
    }));
}

fn is_closed_stdout(message: &str) -> bool {
    message.contains("failed printing to stdout") && message.contains("Broken pipe")
}

fn main() -> anyhow::Result<()> {
    closed_stdout_is_no_crash();
    patch_core::set_git(env!("PATCH_GIT"));
    // Heap profile of this run, written when it ends (diagnostics build).
    #[cfg(feature = "dhat-heap")]
    let _heap = {
        let p = dhat::Profiler::new_heap();
        // PATCH_DHAT_SECS: end the profile after that long, with every
        // window still open (what's live then is what the run holds).
        match std::env::var("PATCH_DHAT_SECS")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
        {
            Some(secs) => {
                std::thread::spawn(move || {
                    std::thread::sleep(std::time::Duration::from_secs(secs));
                    drop(p);
                });
                None
            }
            None => Some(p),
        }
    };
    // PATCH_MTRACE=<from>,<to> seconds (with LD_PRELOAD=libc_malloc_debug.so.0
    // and MALLOC_TRACE=<file>): glibc's trace of every malloc and free, C
    // libraries' included, between those times.
    #[cfg(feature = "dhat-heap")]
    if let Some((from, to)) = std::env::var("PATCH_MTRACE").ok().and_then(|v| {
        let (a, b) = v.split_once(',')?;
        Some((a.parse::<u64>().ok()?, b.parse::<u64>().ok()?))
    }) {
        unsafe extern "C" {
            fn mtrace();
            fn muntrace();
        }
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_secs(from));
            // SAFETY: glibc's tracing switches.
            unsafe { mtrace() };
            std::thread::sleep(std::time::Duration::from_secs(to - from));
            unsafe { muntrace() };
        });
    }
    // Never let a buffer swap wait for vblank. With the display off (DPMS) or
    // a window on a hidden workspace, the compositor sends no frame
    // callbacks and a blocking swap (NVIDIA, Mesa) stalls the UI thread, so
    // the session stops answering. Slint paces frames itself.
    // SAFETY: first thing in main, before any thread exists.
    unsafe {
        std::env::set_var("__GL_SYNC_TO_VBLANK", "0");
        std::env::set_var("vblank_mode", "0");
    }
    let args: Vec<String> = dj_cli::offline_style(std::env::args().skip(1).collect());
    let a: Vec<&str> = args.iter().map(String::as_str).collect();
    let launch = match a.as_slice() {
        ["--version" | "-V" | "version"] => {
            println!("omarchy-patch {}", patch_core::version_string());
            return Ok(());
        }
        [] | ["controller"] => Launch::Controller,
        // The layout on screen (docs/layouts.md: "Your own layouts").
        ["layout", "show"] => return layout_request(Request::LayoutShow),
        ["layout", "reset"] => return layout_request(Request::LayoutReset),
        ["layout", "close"] => return layout_request(Request::LayoutClose),
        ["layout", "save", name @ ..] => {
            let name = name.join(" ");
            return layout_request(Request::LayoutSave((!name.is_empty()).then_some(name)));
        }
        ["layout", "delete", id] => {
            // Without a Patch session, the file alone.
            if send(Request::Ping).is_some() {
                return layout_request(Request::LayoutDelete(id.to_string()));
            }
            if !patch_core::user_layouts::delete(id)? {
                anyhow::bail!("there is no saved layout '{id}'");
            }
            return Ok(());
        }
        // The default layout (chosen at install; `setup layout <id>`).
        ["layout"] => Launch::Layout(patch_core::setup::load().layout.id().to_string()),
        ["layout", recipe] => {
            let missing = patch_surfaces::spec::layout_missing(&patch_core::setup::load(), recipe);
            if !missing.is_empty() {
                let names: Vec<&str> = missing
                    .iter()
                    .filter_map(|m| patch_core::setup::MODULES.iter().find(|x| x.0 == *m).map(|x| x.1))
                    .collect();
                anyhow::bail!(
                    "this layout needs {} (switched off here): Ctrl+K, Setup, or omarchy-patch setup {} on",
                    names.join(" and "),
                    missing.join(" on; omarchy-patch setup ")
                );
            }
            if !matches!(*recipe, "show" | "reset" | "close") {
                bar::remember("layout", recipe);
            }
            Launch::Layout(recipe.to_string())
        }
        ["welcome"] => {
            print!("{}", setup_cli::welcome(&patch_core::setup::load(), std::io::IsTerminal::is_terminal(&std::io::stdout())));
            return Ok(());
        }
        ["open", surfaces @ ..] if !surfaces.is_empty() => {
            for s in surfaces.iter() {
                bar::remember("unit", patch_core::instance::base(s));
            }
            Launch::Open(surfaces.iter().map(|s| s.to_string()).collect())
        }
        ["mic", words @ ..] => return mic_cli::run(words),
        ["player", words @ ..] => return player_cli::run(words),
        ["start"] => Launch::Headless,
        ["close", s] => {
            send(Request::Close(s.to_string()));
            return Ok(());
        }
        // Toggle a window; with Patch not running, start it with that
        // window (a global keybind works either way).
        ["toggle", s] => match send(Request::Toggle(s.to_string())) {
            Some(_) => return Ok(()),
            None => Launch::Open(vec![s.to_string()]),
        },
        ["status"] => {
            println!(
                "{}",
                send(Request::Status).unwrap_or_else(|| "closed".into())
            );
            return Ok(());
        }
        // The shared real-time engine: a detached process started by Patch.
        ["engine"] => {
            // Network links switched on start again with the audio service
            // (each is its own service; ones still running are left alone).
            // Once Patch's nodes exist, so a send links on its first try.
            let _ = std::thread::Builder::new().name("patch-net-restore".into()).spawn(|| {
                std::thread::sleep(std::time::Duration::from_secs(3));
                if let Ok(exe) = std::env::current_exe() {
                    for e in patch_network::ops::restore(&exe) {
                        eprintln!("network: {e}");
                    }
                }
            });
            return patch_engine::daemon::run(&patch_engine::daemon::CHANNELS)
                .map_err(anyhow::Error::msg);
        }
        // Debug: the AI DJ's candidate pool, step by step (as the engine builds it).
        ["dj", "rehearse", a, b, rest @ ..] => return dj_ai_cli::rehearse(a, b, rest),
        // Dev/benchmark: separate a folder's tracks into the stem cache, one
        // at a time (what a set requests on demand for its planned pairs).
        ["dj", "prepare-stems", folder, rest @ ..] => return dj_cli::prepare_stems(folder, rest),
        // Dev/benchmark: the AI's layered arrangements (mashups) for random
        // pairs from a folder with prepared stems, played offline and judged.
        ["dj", "rehearse-arrangements", folder, rest @ ..] => return dj_ai_cli::rehearse_arrangements(folder, rest),
        // Dev: for random pairs from a folder, how the AI would mix them and
        // whether it would ask for their stems (and why).
        ["dj", "stem-need", folder, rest @ ..] => return dj_ai_cli::stem_need(folder, rest),
        ["dj", "rehearse-many", folder, rest @ ..] => return dj_ai_cli::rehearse_many(folder, rest),
        ["dj", "train", folder, rest @ ..] => return dj_ai_cli::train(folder, rest),
        ["dj", "reports"] => return dj_ai_cli::reports(),
        ["dj", "rehearse-report", file, rest @ ..] => return dj_ai_cli::rehearse_report(file, rest),
        ["dj", "ai", "pool"] => return dj_ai_cli::ai_pool(),
        ["dj", "style", rest @ ..] => return dj_cli::style(rest),
        ["dj", "tempo", rest @ ..] => return dj_cli::tempo(rest),
        ["dj", "play-length", rest @ ..] => return dj_cli::play_length(rest),
        ["dj", "stems", rest @ ..] => return dj_cli::stem_prep(rest),
        ["dj", "levels"] => return dj_cli::levels(None),
        ["dj", "levels", mode] => return dj_cli::levels(Some(mode)),
        // The library's scanner and analyser (the engine starts it).
        ["librarian"] => return patch_engine::librarian::run().map_err(anyhow::Error::msg),
        ["engine", "status"] => {
            match patch_engine::client::call(&serde_json::json!({"op": "status"})) {
                Ok(v) => println!("{}", serde_json::to_string_pretty(&v)?),
                Err(e) => println!("{e}"),
            }
            return Ok(());
        }
        // Performance: Patch against the native audio layer, averaged.
        ["perf", rest @ ..] => return perf_cli::run(rest, send),
        // Patch in the Omarchy bar's audio dropdown.
        ["bar", "install"] => return bar::install(),
        // Never use the stock output helper here: it moves Patch's players
        // straight to a device, bypassing the mixer and its listening volume.
        ["output", "set-default", name] => {
            // "Patch" while system audio is off: the controller's system
            // audio switch (every app through Patch, onto the current device).
            if *name == patch_engine::links::BUS && !patch_core::setup::load().system_audio {
                return setup_cli::run(&["system_audio", "on"]);
            }
            patch_engine::client::ensure_running(&std::env::current_exe()?)
                .map_err(anyhow::Error::msg)?;
            patch_engine::client::call(&serde_json::json!({
                "op": "output_select", "sink": name
            }))
            .map_err(anyhow::Error::msg)?;
            return Ok(());
        }
        // The bar's input rows: set the input without moving Patch's own
        // captures (Omarchy's helper moves every source-output, which stutters
        // the output).
        ["input", "set-default", name] => {
            patch_engine::client::ensure_running(&std::env::current_exe()?).map_err(anyhow::Error::msg)?;
            patch_engine::client::call(&serde_json::json!({"op":"input_select", "source":name})).map_err(anyhow::Error::msg)?;
            return Ok(());
        }
        ["input", "mix", mode @ ("main" | "inputs")] => {
            patch_engine::client::call(&serde_json::json!({"op":"input_mix", "main":*mode == "main"})).map_err(anyhow::Error::msg)?;
            return Ok(());
        }
        ["input", "monitor", channel, mode @ ("on" | "off")] => {
            patch_engine::client::call(&serde_json::json!({"op":"mic_monitor", "channel":channel, "on":*mode == "on"})).map_err(anyhow::Error::msg)?;
            return Ok(());
        }
        ["bar", "uninstall"] => return bar::uninstall(),
        ["bar", "health", mode @ ("on" | "off")] => return bar::health(*mode == "on"),
        ["health"] => return bar::health_status(),
        // The actions on the dropdown's rows: pause or end a set, stop a take.
        ["bar", "do", words @ ..] => return bar::act_on(words),
        ["bar", "status"] => {
            println!("{}", bar::status(send(Request::Status)));
            return Ok(());
        }
        // EQ: every target at a glance, and the presets.
        ["eq"] | ["eq", "status"] => return engine_cli::eq(),
        ["eq", "presets"] => return engine_cli::eq_presets(),
        // A headphone correction from AutoEq, as a preset (network only here).
        ["eq", "autoeq", rest @ ..] => return autoeq_cli::run(rest),
        // Output latency: show, measure (acoustic loop) or forget.
        ["latency", rest @ ..] => return engine_cli::latency(rest),
        // Stop the engine (the bus falls back to a plain loopback). Refused
        // while audio flows through it, unless forced.
        ["engine", "quit", rest @ ..] => return engine_cli::engine_quit(rest),
        // Start the Patch service with the desktop session (systemd user
        // unit). systemd only starts it: the engine detaches and supervises
        // and upgrades itself, so handovers stay gapless.
        ["service", what] => return service(what),
        ["setup", rest @ ..] => return setup_cli::run(rest),
        ["modules" | "units", rest @ ..] => return modules_cli::run(rest),
        ["licence" | "license", rest @ ..] => return licence_cli::run(rest),
        // Measure the DSP kernels on this machine and keep the best
        // verified one per op (ADR-004). Real numbers only.
        // Find new music under ~/Music (or a folder) and add it to the
        // library; analysis runs in Patch's background (or `analyze`).
        ["library", "collection", "list"] => {
            let db = patch_library::open_default().map_err(anyhow::Error::msg)?;
            for (id, name, count) in db.collections() {
                println!("{id}\t{name}\t{count} tracks");
            }
            return Ok(());
        }
        ["library", "collection", "create", name, dir] => {
            let db = patch_library::open_default().map_err(anyhow::Error::msg)?;
            let (added, report) = db
                .collection_from_folder(name, std::path::Path::new(dir))
                .map_err(anyhow::Error::msg)?;
            println!(
                "collection '{name}': {added} tracks · scan added {}, moved {}, unchanged {}",
                report.added, report.moved, report.unchanged
            );
            return Ok(());
        }
        // A collection of tracks already in the library, by file: their
        // analysis, stems and history stay theirs (a folder of links would
        // be new tracks).
        ["library", "collection", "add", name, files @ ..] if !files.is_empty() => return media_cli::library_collection_add(name, files),
        ["library", "scan", dir @ ..] => return media_cli::library_scan(dir),
        // Sources and where their tracks stand (after scan/collection above).
        ["library", rest @ ..] => return library_cli::run(rest),
        ["help"] | ["--help"] | ["-h"] => {
            println!("{USAGE}");
            return Ok(());
        }
        ["optimize", rest @ ..] => return engine_cli::optimize(rest),
        // Replace the running engine with this build, without a gap.
        ["engine", "upgrade", rest @ ..] => {
            let exe = std::env::current_exe()?;
            let force = rest.contains(&"--force");
            let stdout = std::io::stdout();
            let mut output = stdout.lock();
            let upgraded = patch_engine::client::upgrade(&exe, force, &mut |m| {
                // A shell pipeline may close stdout early. Progress output
                // must not panic and abort an otherwise successful handover.
                let _ = writeln!(output, "{m}");
            });
            // The bar's widget comes with the build too, engine handed
            // over or not (a release update runs only this command).
            if let Err(e) = bar::refresh() {
                let _ = writeln!(output, "the bar's Patch widget stays as it was: {e}");
            }
            upgraded.map_err(anyhow::Error::msg)?;
            return Ok(());
        }
        ["engine", "start"] => {
            let exe = std::env::current_exe()?;
            patch_engine::client::ensure_running(&exe).map_err(anyhow::Error::msg)?;
            // At login: downloads a reboot interrupted carry on.
            modules_cli::resume_pending();
            modules_cli::spawn_detached(&["licence", "refresh-auto"]);
            println!("running");
            return Ok(());
        }
        ["engine", "stop", rest @ ..] if rest.is_empty() || rest == ["--force"] => return engine_cli::engine_stop(rest),
        ["analyze", files @ ..] if !files.is_empty() => return engine_cli::analyze(files),
        ["dj", "package", "create", path, name]
        | ["dj", "package", "create", path, name, "library"] => {
            let mode = patch_library::PackageMode::Library;
            let p = patch_library::Package::create(std::path::Path::new(path), name, mode)
                .map_err(anyhow::Error::msg)?;
            println!(
                "created {} ({})",
                p.manifest().name,
                p.manifest().package_id
            );
            return Ok(());
        }
        ["dj", "package", "create", path, name, "portable"]
        | ["dj", "package", "create", path, name, "set"]
        | ["dj", "package", "create", path, name, "archive"] => return dj_cli::package_create(&a, path, name),
        ["dj", "package", "validate", path] => return dj_cli::package_validate(path),
        ["dj", "package", "inspect", path] => return dj_cli::package_inspect(path),
        ["dj", "package", "migrate", path] => {
            let p = patch_library::Package::open(std::path::Path::new(path))
                .map_err(anyhow::Error::msg)?;
            p.checkpoint().map_err(anyhow::Error::msg)?;
            println!(
                "migrated to package format {} schema {}",
                p.manifest().format_version,
                p.manifest().schema_version
            );
            return Ok(());
        }
        ["dj", "package", "relink", package, track_uuid, audio] => {
            let p = patch_library::Package::open(std::path::Path::new(package))
                .map_err(anyhow::Error::msg)?;
            let id = p
                .library
                .relink(track_uuid, std::path::Path::new(audio))
                .map_err(anyhow::Error::msg)?;
            println!("relinked track {id} ({track_uuid})");
            return Ok(());
        }
        ["dj", "package", "add", package, audio]
        | ["dj", "package", "add", package, audio, "--embed"] => return dj_cli::package_add(&a, package, audio),
        ["quit"] => {
            send(Request::Quit);
            return Ok(());
        }
        ["plugins", words @ ..] => return plugins_cli::run(words),
        ["shots", rest @ ..] => return media_cli::shots(rest),
        ["record-daemon", source, fmt, rate, path, label] => return media_cli::record_daemon(source, fmt, rate, path, label),
        ["record", "stop"] => {
            match patch_record::daemon::stop_and_wait() {
                Ok(_) => println!("stopped"),
                Err(error) if error == "not recording" => println!("not recording"),
                Err(error) => anyhow::bail!(error),
            }
            return Ok(());
        }
        ["record", verb @ ("pause" | "resume")] => return media_cli::record(verb),
        ["record", "status"] => {
            match patch_record::daemon::status() {
                Some(s) => println!("{s}"),
                None => println!("not recording"),
            }
            return Ok(());
        }
        ["controllers", action] if matches!(*action, "status" | "reload") => {
            let op = if *action == "reload" { "controllers_reload" } else { "controllers" };
            let value = patch_engine::client::call(&serde_json::json!({"op":op})).map_err(anyhow::Error::msg)?;
            println!("{}", serde_json::to_string_pretty(&value)?);
            return Ok(());
        }
        // Convert a mapping the user obtained themselves. Nothing is bundled
        // with Patch and nothing is installed without asking: the profile goes
        // to stdout unless --write says otherwise.
        ["controllers", "import", file] | ["controllers", "import", file, "--write"] => {
            let write = a.len() == 4;
            let text = std::fs::read_to_string(file)
                .map_err(|e| anyhow::anyhow!("{file}: {e}"))?;
            // By name: a modern Mixxx preset says nothing about its transport in
            // the document, and the name is what Mixxx's own loader reads.
            let imported =
                patch_controller::mixxx::import_named(&text, file).map_err(anyhow::Error::msg)?;
            println!("{}", imported.report.summary());
            for entry in imported
                .report
                .unsupported
                .iter()
                .chain(&imported.report.outputs_unsupported)
            {
                println!("  not translated: {} x{} · {}", entry.what, entry.count, entry.reason);
            }
            for warning in &imported.report.warnings {
                println!("  note: {warning}");
            }
            if !write {
                println!("{}", serde_json::to_string_pretty(&imported.profile)?);
                println!(
                    "  add this to the \"profiles\" array in {}, or re-run with --write",
                    patch_controller::config_path().display()
                );
                return Ok(());
            }
            let path = patch_controller::config_path();
            let mut config = patch_controller::Config::load().map_err(anyhow::Error::msg)?;
            if let Some(existing) = config
                .profiles
                .iter()
                .find(|profile| profile.id == imported.profile.id)
            {
                anyhow::bail!(
                    "{} already has a profile with id '{}' ({}) · edit it by hand or remove it first",
                    path.display(),
                    existing.id,
                    existing.label()
                );
            }
            config.profiles.push(imported.profile);
            config.validate().map_err(anyhow::Error::msg)?;
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            // Write beside the file and rename, so a failure cannot leave a
            // half-written profile list behind.
            let staged = path.with_extension("json.new");
            std::fs::write(&staged, serde_json::to_vec_pretty(&config)?)?;
            std::fs::rename(&staged, &path)?;
            println!(
                "wrote {} · {} profiles · reload with: omarchy-patch controllers reload",
                path.display(),
                config.profiles.len()
            );
            return Ok(());
        }
        ["broadcast", "status"] => {
            match patch_broadcast::daemon::status() {
                Some(s) => println!("{}", serde_json::to_string_pretty(&s)?),
                // A broadcast that failed says why (its outcome is kept
                // until the next one starts).
                None => match patch_broadcast::daemon::last_outcome() {
                    Some(o) if o["error"].as_str().is_some_and(|e| !e.is_empty()) => {
                        let ago = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map_or(0.0, |d| d.as_secs_f64())
                            - o["ended"].as_f64().unwrap_or(0.0);
                        let ago = if ago < 120.0 {
                            format!("{:.0} s ago", ago.max(0.0))
                        } else {
                            format!("{:.0} min ago", ago / 60.0)
                        };
                        println!(
                            "not broadcasting · the last broadcast failed ({ago}): {}",
                            o["error"].as_str().unwrap_or_default()
                        );
                    }
                    _ => println!("not broadcasting"),
                },
            }
            return Ok(());
        }
        ["broadcast", "stop"] => {
            match patch_broadcast::daemon::stop_and_wait() {
                Ok(r) => println!("{}", serde_json::to_string_pretty(&r)?),
                Err(error) if error == "not broadcasting" => println!("not broadcasting"),
                Err(error) => anyhow::bail!(error),
            }
            return Ok(());
        }
        ["broadcast", "start", kind, target, rest @ ..] => {
            let config = broadcast_config(kind, target, rest)?;
            let exe = std::env::current_exe()?;
            let asked = patch_broadcast::daemon::start(&exe, &config).map_err(anyhow::Error::msg)?;
            // On air only once the destination takes the stream; a refused
            // one says why.
            let s = patch_broadcast::daemon::wait_until_live(asked, std::time::Duration::from_secs(12))
                .map_err(|e| anyhow::anyhow!("the broadcast failed: {e}"))?;
            let to = s["target"].as_str().unwrap_or_default();
            if s["state"] == "live" {
                println!("broadcasting to {to}");
            } else {
                println!("connecting to {to} (omarchy-patch broadcast status)");
            }
            // A window on screen starts sending within a second or two.
            let fed = || {
                (0..25).any(|_| {
                    std::thread::sleep(std::time::Duration::from_millis(100));
                    patch_broadcast::daemon::status().is_some_and(|s| s["picture_live"] == true)
                })
            };
            if let Some(p) = s["picture"].as_str().and_then(patch_broadcast::video::Picture::parse)
                && s["picture_live"] != true
                && !fed()
            {
                println!(
                    "no {} on screen: the picture is black until one is (omarchy-patch open {})",
                    p.label(),
                    p.surface()
                );
            }
            return Ok(());
        }
        // The Network Unit: sends, receives, VBAN (patch-network).
        ["network", rest @ ..] => return network_cli::run(rest),
        ["broadcast-daemon"] => {
            let json = std::env::var(patch_broadcast::daemon::CONFIG_ENV)
                .map_err(|_| anyhow::anyhow!("no broadcast config"))?;
            patch_broadcast::daemon::run_from_json(&json).map_err(anyhow::Error::msg)?;
            return Ok(());
        }
        // A daemon started by an older Patch (config on the command line).
        ["broadcast-daemon", json] => {
            patch_broadcast::daemon::run_from_json(json).map_err(anyhow::Error::msg)?;
            return Ok(());
        }
        // Derived Material: omarchy-patch material stats|cleanup|state <id>|
        // request <recipe.json>|release <id> [consumer]. The engine cuts,
        // transforms and caches; the recipe is Patch data.
        ["material", rest @ ..] => return media_cli::material(rest),
        // Visual intent (what the picture should mean): omarchy-patch
        // visual-intent [get|clear|set field=value …], e.g.
        // `visual-intent set section=drop intensity=0.9 hold_s=32`.
        ["visual-intent", rest @ ..] => return media_cli::visual_intent(rest),
        // The Pattern Unit: omarchy-patch pattern status|describe|events [from] [to]|
        // timeline [visual]|play|stop|tempo <bpm>|follow <auto|a|b|c|d>|
        // engine <auto|strudel|reference>|load <file> [patch|strudel].
        ["pattern", rest @ ..] => return media_cli::pattern(rest),
        // The Tidal Unit: omarchy-patch tidal eval <code|->|run <file>|hush|
        // play|stop|status|tempo <bpm>|follow <auto|a|b|c|d>|events [from] [to].
        ["tidal", rest @ ..] => return media_cli::tidal(rest),
        ["hypr-rules"] => {
            print!("{}", patch_host::hypr::RULES_LUA);
            return Ok(());
        }
        // Everything else is a Patch command: parsed, sent as JSON, and
        // validated by the running session before it touches audio.
        words => match Command::parse_cli(words) {
            Ok(cmd) => {
                let reply = send(Request::Command {
                    origin: Origin::Cli,
                    json: cmd.to_json(),
                });
                // Without the UI, the service runs what is its own (DJ
                // commands, transitions): no window is needed for those.
                let reply = reply.unwrap_or_else(|| {
                    let req = match &cmd {
                        Command::DjPlan { plan, level } => serde_json::json!({"op": "plan", "plan": plan, "level": level, "origin": "cli"}),
                        Command::DjPlanCancel => serde_json::json!({"op": "plan_cancel"}),
                        // The master gain is the engine's own.
                        Command::SetGain { strip, db } if strip.eq_ignore_ascii_case("master") => serde_json::json!({"op": "master", "db": db}),
                        c => serde_json::json!({"op": "command", "command": serde_json::from_str::<serde_json::Value>(&c.to_json()).unwrap_or_default(), "origin": "cli"}),
                    };
                    match patch_engine::client::call(&req) {
                        Ok(_) => "ok".into(),
                        Err(e) if e.contains("not a service command") => "Patch isn't running (this command needs the Patch UI)".into(),
                        Err(e) => e,
                    }
                });
                println!("{reply}");
                if reply != "ok" {
                    std::process::exit(1);
                }
                return Ok(());
            }
            Err(_) => {
                eprintln!("{USAGE}");
                std::process::exit(2);
            }
        },
    };
    // A window opens: a module key is checked (at most daily, offline
    // changes nothing).
    if !matches!(launch, Launch::Headless) {
        modules_cli::spawn_detached(&["licence", "refresh-auto"]);
    }
    patch_host::run::<AudioApp>(launch)
}

/// The Patch service with the desktop session (systemd user unit).
/// systemd only starts it: the engine detaches and supervises and upgrades
/// itself, so handovers stay gapless.
pub fn service(what: &str) -> anyhow::Result<()> {
    let dir = std::env::var_os("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            std::path::PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config")
        })
        .join("systemd/user");
    let unit = dir.join("omarchy-patch.service");
    let systemctl = |args: &[&str]| {
        std::process::Command::new("systemctl")
            .arg("--user")
            .args(args)
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    };
    match what {
        "install" => {
            // The command as you run it (~/.local/bin survives rebuilds).
            let exe = bar::invoked().unwrap_or(std::env::current_exe()?);
            std::fs::create_dir_all(&dir)?;
            // Part of PipeWire, like WirePlumber: it starts when PipeWire
            // does (at login) and restarts with it. Headless: the minimal
            // Patch, the audio layer; windows open when you ask.
            std::fs::write(
                &unit,
                format!(
                    "[Unit]\nDescription=Patch audio service (the Omarchy audio layer)\nAfter=pipewire.service wireplumber.service\nRequires=pipewire.service\nPartOf=pipewire.service\n\n[Service]\nType=oneshot\nRemainAfterExit=yes\nExecStart={exe} engine start\n\n[Install]\nWantedBy=pipewire.service\n",
                    exe = exe.display()
                ),
            )?;
            // An older unit hooked to the graphical session: unhook it.
            systemctl(&["disable", "omarchy-patch.service"]);
            // Started now too (a no-op when the engine runs already): else
            // it reads "inactive" until the next login.
            let ok = systemctl(&["daemon-reload"])
                && systemctl(&["enable", "--now", "omarchy-patch.service"]);
            println!(
                "{} {}",
                if ok {
                    "installed and enabled:"
                } else {
                    "written (systemctl failed; enable it by hand):"
                },
                unit.display()
            );
        }
        "uninstall" => {
            systemctl(&["disable", "omarchy-patch.service"]);
            let _ = std::fs::remove_file(&unit);
            systemctl(&["daemon-reload"]);
            println!("removed: {}", unit.display());
        }
        "status" => {
            println!(
                "unit: {} ({})",
                unit.display(),
                if unit.exists() {
                    "installed"
                } else {
                    "not installed"
                }
            );
            println!(
                "engine: {}",
                if patch_engine::client::is_running() {
                    "running"
                } else {
                    "not running"
                }
            );
        }
        _ => anyhow::bail!("service install|uninstall|status"),
    }
    Ok(())
}

#[cfg(test)]
mod pipe_tests {
    #[test]
    fn only_a_closed_stdout_ends_quietly() {
        assert!(super::is_closed_stdout("failed printing to stdout: Broken pipe (os error 32)"));
        assert!(!super::is_closed_stdout("failed printing to stdout: No space left on device"));
        assert!(!super::is_closed_stdout("index out of bounds"));
    }
}
