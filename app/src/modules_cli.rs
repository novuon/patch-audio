//! `omarchy-patch modules`: adding and removing what Patch has besides its
//! core, for the installer, Settings and the terminal.
//!
//! A module is a switch in `setup.toml` (its Units show), files it
//! downloads (the themes, the wallpapers), or an install step (the stem
//! separator). Downloads come from the release's signed manifest and run in
//! a background worker of their own (`modules worker`, nice 19, idle I/O),
//! never in the engine: they resume after a dropped connection or a
//! reboot, are checked against the manifest's SHA-256 and move into place
//! only when whole. A module's switch turns on once its files are there.

use anyhow::{Context, Result, bail};
use patch_core::modules::{self, Catalogue, Module, Payload, human_size};
use serde::Serialize;
use std::io::{IsTerminal, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// The release key's public half (Ed25519). The private half signs each
/// release's `manifest.json` (`scripts/build-modules.sh`).
const PUBLIC_KEY: &str = "32358604c4535b6878cb4c55ccb0c15dff9957a567c55d12c6b07cad0b0eaef5";

/// Over this, a download asks first and runs in the background.
const BIG: u64 = 50_000_000;
/// A foreground download that would take longer than this moves to the
/// background.
const PATIENCE: Duration = Duration::from_secs(30);

pub fn run(args: &[&str]) -> Result<()> {
    let json = args.contains(&"--json");
    let yes = args.contains(&"--yes") || args.contains(&"-y");
    let mode = if args.contains(&"--background") {
        Mode::Background
    } else if args.contains(&"--now") {
        Mode::Now
    } else {
        Mode::Auto
    };
    let words: Vec<&str> = args.iter().copied().filter(|a| !a.starts_with('-')).collect();
    match words.as_slice() {
        [] | ["list"] => list(json),
        ["profiles"] => profiles(json),
        ["status"] => status(json),
        ["add", ids @ ..] if !ids.is_empty() => add(ids, yes, mode),
        ["remove", ids @ ..] if !ids.is_empty() => remove(ids, yes),
        ["pause"] => set_paused(true),
        ["resume"] => {
            set_paused(false)?;
            spawn_worker()
        }
        ["cancel", ids @ ..] => cancel(ids),
        ["worker"] => worker(),
        ["resume-pending"] => {
            resume_pending();
            Ok(())
        }
        ["manifest"] => {
            let c = manifest()?;
            println!("manifest for v{}: {} modules, verified", c.version, c.modules.len());
            Ok(())
        }
        _ => {
            println!(
                "omarchy-patch modules [list] [--json]          what is installed and what can be added
omarchy-patch modules profiles [--json]        the install profiles and their modules
omarchy-patch modules add <id>… [--yes] [--now|--background]
omarchy-patch modules remove <id>… [--yes]     removes and deletes its files (asks first)
omarchy-patch modules status [--json]          downloads in progress
omarchy-patch modules pause|resume|cancel [<id>…]"
            );
            Ok(())
        }
    }
}

/// At login (the service): downloads a reboot or a dropped session
/// interrupted carry on, unless paused.
pub fn resume_pending() {
    let j = load_jobs();
    if !j.paused && j.jobs.iter().any(|x| x.active()) {
        let _ = spawn_worker();
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Auto,
    Now,
    Background,
}

// ---------------------------------------------------------------- places

use patch_core::module_state::{
    Installed, Job, Jobs, cache_dir, data_dir, dest_root, installed_path, is_on, jobs_path, load_installed,
    load_jobs, present, state_dir, stems_runtime,
};

fn save_installed(i: &Installed) -> Result<()> {
    write_atomic(&installed_path(), serde_json::to_string_pretty(i)?.as_bytes())
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// Change the job list under its lock (the worker and the CLI both write it).
fn with_jobs<T>(f: impl FnOnce(&mut Jobs) -> T) -> Result<T> {
    let _lock = Lock::take(&state_dir().join("jobs.lock"), true)?;
    let mut j = load_jobs();
    let out = f(&mut j);
    write_atomic(&jobs_path(), serde_json::to_string_pretty(&j)?.as_bytes())?;
    Ok(out)
}

fn update_job(id: &str, f: impl FnOnce(&mut Job)) -> Result<Option<Job>> {
    with_jobs(|j| {
        let job = j.jobs.iter_mut().find(|x| x.id == id)?;
        f(job);
        Some(job.clone())
    })
}

/// An flock on a file, released when dropped.
struct Lock {
    _file: std::fs::File,
}

impl Lock {
    /// `wait`: block until free; else None when someone holds it.
    fn take(path: &Path, wait: bool) -> Result<Lock> {
        Lock::try_take(path, wait)?.context("locked")
    }

    fn try_take(path: &Path, wait: bool) -> Result<Option<Lock>> {
        use std::os::fd::AsRawFd;
        if let Some(d) = path.parent() {
            std::fs::create_dir_all(d)?;
        }
        let f = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(path)?;
        let op = if wait { libc::LOCK_EX } else { libc::LOCK_EX | libc::LOCK_NB };
        // SAFETY: flock on a file descriptor we own; no memory is shared.
        let r = unsafe { libc::flock(f.as_raw_fd(), op) };
        Ok((r == 0).then_some(Lock { _file: f }))
    }
}

// ---------------------------------------------------------------- manifest

fn manifest_urls() -> Vec<String> {
    if let Ok(u) = std::env::var("PATCH_MANIFEST_URL") {
        return vec![u];
    }
    let v = patch_core::VERSION;
    vec![
        format!("https://patchaudio.app/releases/v{v}/manifest.json"),
        format!("https://github.com/novuon/patch-releases/releases/download/v{v}/manifest.json"),
    ]
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(s: &str) -> Option<Vec<u8>> {
    (0..s.len())
        .step_by(2)
        .map(|i| s.get(i..i + 2).and_then(|b| u8::from_str_radix(b, 16).ok()))
        .collect()
}

/// Check `body` against its signature (base64 or hex of 64 bytes).
fn verify(body: &[u8], sig: &str, key: &str) -> Result<()> {
    let sig = sig.trim();
    let raw = unhex(sig).filter(|b| b.len() == 64).or_else(|| base64(sig)).context("signature unreadable")?;
    let key = ed25519_compact::PublicKey::from_slice(&unhex(key).context("key")?).map_err(|e| anyhow::anyhow!("{e}"))?;
    let sig = ed25519_compact::Signature::from_slice(&raw).map_err(|e| anyhow::anyhow!("{e}"))?;
    key.verify(body, &sig).map_err(|_| anyhow::anyhow!("the manifest's signature does not match Patch's release key"))
}

fn base64(s: &str) -> Option<Vec<u8>> {
    const A: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = Vec::new();
    let (mut acc, mut bits) = (0u32, 0);
    for c in s.bytes().filter(|c| !c.is_ascii_whitespace() && *c != b'=') {
        acc = (acc << 6) | A.iter().position(|a| *a == c)? as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Some(out)
}

fn curl_get(url: &str, to: &Path) -> Result<()> {
    let ok = Command::new("curl")
        // Quiet: a missing mirror (patchaudio.app before GitHub) is tried
        // and passed over, not shown.
        .args(["-fsL", "--connect-timeout", "15", "--max-time", "60", "-o"])
        .arg(to)
        .arg(url)
        .status()
        .context("curl")?
        .success();
    if !ok {
        bail!("{url} could not be fetched");
    }
    Ok(())
}

/// The release's manifest, signed: fetched (and kept), or the kept copy
/// when offline.
fn manifest() -> Result<Catalogue> {
    let dir = state_dir();
    std::fs::create_dir_all(&dir)?;
    let (kept, kept_sig) = (dir.join("manifest.json"), dir.join("manifest.json.sig"));
    let mut last = None;
    for url in manifest_urls() {
        let (body, sig) = (dir.join("manifest.new"), dir.join("manifest.new.sig"));
        let got = curl_get(&url, &body).and_then(|_| curl_get(&format!("{url}.sig"), &sig)).and_then(|_| {
            let c = read_manifest(&body, &sig)?;
            std::fs::rename(&body, &kept)?;
            std::fs::rename(&sig, &kept_sig)?;
            Ok(c)
        });
        match got {
            Ok(c) => return Ok(c),
            Err(e) => last = Some(e),
        }
    }
    read_manifest(&kept, &kept_sig).map_err(|e| last.unwrap_or(e)).context("the release's module list is not reachable (offline?)")
}

fn read_manifest(body: &Path, sig: &Path) -> Result<Catalogue> {
    let bytes = std::fs::read(body)?;
    let key = std::env::var("PATCH_MANIFEST_KEY").ok().filter(|_| modules::dev_overrides()).unwrap_or_else(|| PUBLIC_KEY.into());
    verify(&bytes, &std::fs::read_to_string(sig)?, &key)?;
    let c = modules::parse(std::str::from_utf8(&bytes)?).map_err(anyhow::Error::msg)?;
    if c.version != patch_core::VERSION {
        bail!("the manifest is for v{}, this Patch is v{}", c.version, patch_core::VERSION);
    }
    Ok(c)
}

// ---------------------------------------------------------------- list

#[derive(Serialize)]
struct Row<'a> {
    id: &'a str,
    name: &'a str,
    about: &'a str,
    group: &'a str,
    preview: bool,
    needs: &'a [String],
    /// on, off, queued, downloading, verifying, installing, failed
    state: String,
    progress: Option<f32>,
    error: Option<String>,
    /// Download still needed to add it (0 when its files are here).
    size: u64,
    /// A module without its key here.
    locked: bool,
    /// Where to get its key.
    key_url: Option<&'a str>,
}

fn rows<'a>(c: &'a Catalogue, sizes: Option<&Catalogue>) -> Vec<Row<'a>> {
    let s = patch_core::setup::load();
    let installed = load_installed();
    let jobs = load_jobs();
    c.modules
        .iter()
        .filter(|m| !m.hidden)
        .map(|m| {
            let job = jobs.jobs.iter().find(|j| j.id == m.id && (j.active() || j.state == "failed"));
            let state = match job {
                Some(j) => j.state.clone(),
                None if is_on(m, &s, &installed) => "on".into(),
                None => "off".into(),
            };
            let size = if present(m, &installed) {
                0
            } else {
                sizes.unwrap_or(c).download_size(&m.id, modules::arch())
            };
            Row {
                id: &m.id,
                name: &m.name,
                about: &m.about,
                group: &m.group,
                preview: m.preview,
                needs: &m.needs,
                progress: job.filter(|j| j.total > 0).map(|j| j.done as f32 / j.total as f32),
                error: job.and_then(|j| j.error.clone()),
                state,
                size,
                locked: !patch_core::licence::module_unlocked(&m.id),
                key_url: patch_core::licence::for_module(c, &m.id).map(|l| l.offer_url()),
            }
        })
        .collect()
}

fn list(json: bool) -> Result<()> {
    let c = modules::builtin();
    // Sizes from the kept manifest when there is one (no network here).
    let dir = state_dir();
    let kept = read_manifest(&dir.join("manifest.json"), &dir.join("manifest.json.sig")).ok();
    let rows = rows(c, kept.as_ref());
    if json {
        println!("{}", serde_json::json!({"core": c.core, "modules": rows}));
        return Ok(());
    }
    println!("● {:<20} {}", c.core.name, c.core.about);
    for r in &rows {
        let mark = match r.state.as_str() {
            "on" => "●",
            "off" => "○",
            _ => "◐",
        };
        let extra = match (r.state.as_str(), r.progress) {
            ("on" | "off", _) if r.size > 0 => format!("  ({})", human_size(r.size)),
            ("on" | "off", _) => String::new(),
            (st, Some(p)) => format!("  [{st} {:.0} %]", p * 100.0),
            (st, None) => format!("  [{st}{}]", r.error.as_deref().map(|e| format!(": {e}")).unwrap_or_default()),
        };
        let soon = patch_core::licence::for_module(c, r.id).is_some_and(|l| l.coming_soon);
        let preview = match (r.preview, r.locked) {
            (_, true) if soon => " (coming soon)",
            (_, true) => " (needs a key: omarchy-patch licence activate <key>)",
            (true, _) => " (preview)",
            _ => "",
        };
        println!("{mark} {:<20} {}{preview}{extra}", r.name, r.about);
    }
    println!("\nomarchy-patch modules add <id> · remove <id>   ids: {}", c.modules.iter().filter(|m| !m.hidden).map(|m| m.id.as_str()).collect::<Vec<_>>().join(" "));
    Ok(())
}

fn profiles(json: bool) -> Result<()> {
    let c = modules::builtin();
    if json {
        println!("{}", serde_json::to_string(&c.profiles)?);
        return Ok(());
    }
    for p in c.profiles.iter().filter(|p| !p.hidden) {
        println!("{}{:<12} {}", if p.default { "*" } else { " " }, p.id, p.about);
        println!("              ticked: {}   offered: {}", p.modules.join(" "), p.offer.join(" "));
    }
    Ok(())
}

fn status(json: bool) -> Result<()> {
    let j = load_jobs();
    if json {
        println!("{}", serde_json::to_string(&j)?);
        return Ok(());
    }
    let shown: Vec<&Job> = j.jobs.iter().filter(|x| x.state != "done" && x.state != "cancelled").collect();
    if shown.is_empty() {
        println!("Nothing downloading");
    }
    let c = modules::builtin();
    for x in shown {
        let name = c.module(&x.id).map_or(x.id.as_str(), |m| m.name.as_str());
        println!("{name:<22} {}", progress_line(x));
    }
    if j.paused {
        println!("(paused: omarchy-patch modules resume)");
    }
    Ok(())
}

fn progress_line(j: &Job) -> String {
    let pct = if j.total > 0 { j.done as f64 * 100.0 / j.total as f64 } else { 0.0 };
    let left = (j.rate > 0 && j.total > j.done).then(|| eta(Duration::from_secs((j.total - j.done) / j.rate)));
    match (j.state.as_str(), left) {
        ("downloading", Some(l)) => format!("downloading {pct:.0} % · {}/s · {l} left", human_size(j.rate)),
        ("failed", _) => format!("failed: {}", j.error.as_deref().unwrap_or("?")),
        (st, _) => format!("{st} {pct:.0} %"),
    }
}

fn eta(d: Duration) -> String {
    let s = d.as_secs();
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

// ---------------------------------------------------------------- add

fn confirm(question: &str) -> Result<bool> {
    if !std::io::stdin().is_terminal() {
        bail!("{question} (run with --yes to confirm)");
    }
    print!("{question} [y/N] ");
    std::io::stdout().flush()?;
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    Ok(matches!(line.trim(), "y" | "Y" | "yes" | "j" | "ja"))
}

/// The ids a user may name: every module (the hidden ones too, by name:
/// they are only not listed).
fn known(c: &Catalogue, ids: &[&str]) -> Result<()> {
    for id in ids {
        if c.module(id).is_none() {
            bail!(
                "unknown module {id}; modules: {}",
                c.modules.iter().filter(|m| !m.hidden).map(|m| m.id.as_str()).collect::<Vec<_>>().join(" ")
            );
        }
    }
    Ok(())
}

/// What `ids` bring along once they may run (Auto Mix: its stems).
fn with_brought(c: &Catalogue, ids: Vec<String>) -> Vec<String> {
    let mut more: Vec<String> = ids.clone();
    for id in &ids {
        if let Some(m) = c.module(id) {
            more.extend(m.brings.iter().cloned());
        }
    }
    c.with_needs(&more)
}

/// A module that brings others (Auto Mix) is on and may run, and what it
/// brings is missing: added now, in the background. After a key is
/// activated, and at each check of it.
pub fn ensure_brought() {
    let c = modules::builtin();
    let s = patch_core::setup::load();
    let installed = load_installed();
    let jobs = load_jobs();
    for m in c.modules.iter().filter(|m| !m.brings.is_empty() && is_on(m, &s, &installed)) {
        let missing = m.brings.iter().filter_map(|b| c.module(b)).any(|b| {
            !present(b, &installed) && !jobs.jobs.iter().any(|j| j.id == b.id && (j.active() || j.state == "failed"))
        });
        if missing {
            spawn_detached(&["modules", "add", &m.id, "--yes"]);
        }
    }
}

fn add(ids: &[&str], yes: bool, mode: Mode) -> Result<()> {
    let builtin = modules::builtin();
    known(builtin, ids)?;
    let s = patch_core::setup::load();
    let installed = load_installed();
    let mut all = builtin.with_needs(ids);
    // A module without its key: said, and left out; the rest is
    // added (a profile with Stems still gets its themes).
    let locked: Vec<String> = all.iter().filter(|id| !patch_core::licence::module_unlocked(id)).cloned().collect();
    if let Some(l) = locked.first().and_then(|id| patch_core::licence::for_module(builtin, id)) {
        // Chosen all the same: its switch is saved on, and it comes on
        // (with what it brings) when the key is activated.
        let wish: Vec<(&str, bool)> = locked
            .iter()
            .filter(|id| builtin.module(id).is_some_and(|m| m.switch))
            .map(|id| (id.as_str(), true))
            .collect();
        if !wish.is_empty() {
            crate::setup_cli::switch(&wish)?;
        }
        println!("{}", l.chosen(&names(builtin, &locked)));
        all.retain(|id| !locked.contains(id));
        if all.is_empty() {
            return Ok(());
        }
    }
    // What the unlocked modules bring (Auto Mix: its stems) comes too.
    let all = with_brought(builtin, all);
    let todo: Vec<&Module> = all.iter().filter_map(|id| builtin.module(id)).filter(|m| !is_on(m, &s, &installed)).collect();
    if todo.is_empty() {
        println!("Already in: {}", names(builtin, &all));
        return Ok(());
    }
    // What needs fetching, and how much.
    let fetch: Vec<&Module> = todo.iter().copied().filter(|m| (m.download || m.step.is_some()) && !present(m, &installed)).collect();
    let catalogue = if fetch.iter().any(|m| m.download) { manifest()? } else { builtin.clone() };
    let total: u64 = fetch.iter().map(|m| catalogue.download_size(&m.id, modules::arch())).sum();
    for m in fetch.iter().filter(|m| m.download) {
        if catalogue.payloads_for(&m.id, modules::arch()).next().is_none() {
            bail!("{} has no download for {} in this release", m.name, modules::arch());
        }
    }
    let list = todo.iter().map(|m| m.name.as_str()).collect::<Vec<_>>().join(", ");
    if total > 0 {
        println!("Adding {list}: downloads {}", human_size(total));
    } else {
        println!("Adding {list}");
    }
    if total > BIG && !yes && !confirm(&format!("Download {}?", human_size(total)))? {
        println!("Nothing changed");
        return Ok(());
    }
    // Switches whose files are here go on now; the rest when they arrive.
    let now: Vec<&str> = todo.iter().filter(|m| m.switch && !fetch.iter().any(|f| f.id == m.id)).map(|m| m.id.as_str()).collect();
    if !now.is_empty() {
        crate::setup_cli::switch(&now.iter().map(|id| (*id, true)).collect::<Vec<_>>())?;
    }
    if fetch.is_empty() {
        return Ok(());
    }
    with_jobs(|j| {
        for m in &fetch {
            j.jobs.retain(|x| x.id != m.id);
            j.jobs.push(Job {
                id: m.id.clone(),
                state: "queued".into(),
                done: 0,
                total: catalogue.download_size(&m.id, modules::arch()),
                rate: 0,
                error: None,
                tries: 0,
            });
        }
        j.paused = false;
    })?;
    let background = mode == Mode::Background || (mode == Mode::Auto && (total > BIG || !std::io::stdout().is_terminal()));
    if background {
        spawn_worker()?;
        println!("Downloading in the background: Settings · Modules, or `omarchy-patch modules status`. Patch tells you when it is ready.");
        return Ok(());
    }
    // In front, with progress; a slow line moves to the background.
    match work(&catalogue, Some(mode == Mode::Auto)) {
        Ok(Handoff::Done) => Ok(()),
        Ok(Handoff::Background) => {
            spawn_worker()?;
            println!("\nThis takes a while on this connection: it carries on in the background (`omarchy-patch modules status`).");
            Ok(())
        }
        Err(e) => Err(e),
    }
}

fn names(c: &Catalogue, ids: &[String]) -> String {
    ids.iter().filter_map(|id| c.module(id)).map(|m| m.name.as_str()).collect::<Vec<_>>().join(", ")
}

// ---------------------------------------------------------------- remove

fn dir_size(p: &Path) -> u64 {
    let Ok(meta) = std::fs::symlink_metadata(p) else { return 0 };
    if meta.is_dir() {
        std::fs::read_dir(p).map_or(0, |d| d.flatten().map(|e| dir_size(&e.path())).sum())
    } else {
        meta.len()
    }
}

fn remove(ids: &[&str], yes: bool) -> Result<()> {
    let c = modules::builtin();
    known(c, ids)?;
    let s = patch_core::setup::load();
    let mut installed = load_installed();
    let mut going: Vec<String> = Vec::new();
    for id in ids {
        for g in std::iter::once(id.to_string()).chain(c.needed_by(id)) {
            let m = c.module(&g).expect("known");
            let busy = load_jobs().jobs.iter().any(|j| j.id == g && j.active());
            if (is_on(m, &s, &installed) || present(m, &installed) || busy) && !going.contains(&g) {
                going.push(g);
            }
        }
    }
    if going.is_empty() {
        println!("Not installed: {}", ids.join(", "));
        return Ok(());
    }
    let freed: u64 = going
        .iter()
        .map(|id| match installed.modules.get(id) {
            Some(r) => r.files.iter().map(|f| dir_size(f)).sum(),
            None if id == "stems" => dir_size(&stems_runtime()),
            None => 0,
        })
        .sum();
    let extra: Vec<&String> = going.iter().filter(|g| !ids.contains(&g.as_str())).collect();
    let mut q = format!("Remove {}", names(c, &going));
    if !extra.is_empty() {
        q += &format!(" ({} need{} it)", names(c, &extra.iter().map(|s| s.to_string()).collect::<Vec<_>>()), if extra.len() == 1 { "s" } else { "" });
    }
    if freed > 0 {
        q += &format!(" and delete {} of files", human_size(freed));
    }
    q += "?";
    if !yes && !confirm(&q)? {
        println!("Nothing changed");
        return Ok(());
    }
    with_jobs(|j| {
        for x in j.jobs.iter_mut().filter(|x| going.contains(&x.id) && x.active()) {
            x.state = "cancelled".into();
        }
    })?;
    let switches: Vec<(&str, bool)> = going.iter().filter(|id| c.module(id).is_some_and(|m| m.switch)).map(|id| (id.as_str(), false)).collect();
    if !switches.is_empty() {
        crate::setup_cli::switch(&switches)?;
    }
    for id in &going {
        if let Some(r) = installed.modules.remove(id) {
            delete_files(&r.files);
        } else if id == "stems" && stems_runtime().exists() {
            std::fs::remove_dir_all(stems_runtime()).ok();
        } else if c.module(id).is_some_and(|m| m.download) && present(c.module(id).unwrap(), &Installed::default()) {
            println!("  {}: switched off; its files came with an older installer and stay", c.module(id).unwrap().name);
        }
    }
    save_installed(&installed)?;
    println!("✓ removed {}{}", names(c, &going), if freed > 0 { format!(", {} freed", human_size(freed)) } else { String::new() });
    Ok(())
}

/// Delete these files, then the folders they leave empty.
fn delete_files(files: &[PathBuf]) {
    let mut dirs: Vec<PathBuf> = Vec::new();
    for f in files {
        if std::fs::remove_file(f).is_ok()
            && let Some(d) = f.parent()
        {
            dirs.push(d.to_path_buf());
        }
    }
    dirs.sort_by_key(|d| std::cmp::Reverse(d.components().count()));
    dirs.dedup();
    for d in dirs {
        for a in d.ancestors() {
            if a == data_dir() || std::fs::remove_dir(a).is_err() {
                break;
            }
        }
    }
}

fn cancel(ids: &[&str]) -> Result<()> {
    let n = with_jobs(|j| {
        let mut n = 0;
        for x in j.jobs.iter_mut().filter(|x| x.active() && (ids.is_empty() || ids.contains(&x.id.as_str()))) {
            x.state = "cancelled".into();
            n += 1;
        }
        n
    })?;
    // What was downloaded so far goes too.
    if let Ok(d) = std::fs::read_dir(cache_dir()) {
        let c = modules::builtin();
        for e in d.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            let mine = c
                .modules
                .iter()
                .any(|m| (ids.is_empty() || ids.contains(&m.id.as_str())) && name.starts_with(&format!("patch-{}-", m.id)));
            if mine && name.ends_with(".part") {
                std::fs::remove_file(e.path()).ok();
            }
        }
    }
    println!("{n} download{} cancelled", if n == 1 { "" } else { "s" });
    Ok(())
}

fn set_paused(paused: bool) -> Result<()> {
    with_jobs(|j| j.paused = paused)?;
    println!("{}", if paused { "Downloads paused" } else { "Downloads resumed" });
    Ok(())
}

// ---------------------------------------------------------------- worker

/// Start the background worker: its own user service at the lowest
/// priority, or a detached process where there is no systemd.
fn spawn_worker() -> Result<()> {
    let exe = std::env::current_exe()?;
    // The worker sees the same folders and manifest as this command.
    let pass = ["HOME", "XDG_DATA_HOME", "XDG_CONFIG_HOME", "XDG_STATE_HOME", "XDG_CACHE_HOME", "PATCH_MANIFEST_URL", "PATCH_MANIFEST_KEY"];
    let env: Vec<String> = pass.iter().filter(|v| std::env::var_os(v).is_some()).map(|v| format!("--setenv={v}")).collect();
    let systemd = std::env::var_os("PATCH_MODULES_NO_SYSTEMD").is_none();
    let unit = systemd
        && Command::new("systemd-run")
        .args(["--user", "--quiet", "--collect", "--unit=omarchy-patch-modules", "-p", "Nice=19", "-p", "IOSchedulingClass=idle"])
        .args(&env)
        .arg(&exe)
        .args(["modules", "worker"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    if unit {
        return Ok(());
    }
    // A worker runs already (the unit exists), or no systemd: a detached
    // one, which leaves at once when another holds the lock.
    use std::os::unix::process::CommandExt;
    let mut child = Command::new(&exe)
        .args(["modules", "worker"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()?;
    // SAFETY: setpriority on a child we started; no memory involved.
    unsafe {
        libc::setpriority(libc::PRIO_PROCESS, child.id(), 19);
    }
    // Collected when it ends, so no finished process is left behind.
    std::thread::spawn(move || child.wait());
    Ok(())
}

/// Start `omarchy-patch <args>` on its own, at the lowest priority, and
/// don't wait for it.
pub fn spawn_detached(args: &[&str]) {
    use std::os::unix::process::CommandExt;
    let Ok(exe) = std::env::current_exe() else { return };
    if let Ok(mut child) = Command::new(exe)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
    {
        // SAFETY: setpriority on a child we started; no memory involved.
        unsafe {
            libc::setpriority(libc::PRIO_PROCESS, child.id(), 19);
        }
        // Collected when it ends: every window start ran a licence check,
        // and its finished process stayed behind the layout.
        std::thread::spawn(move || child.wait());
    }
}

enum Handoff {
    Done,
    Background,
}

fn worker() -> Result<()> {
    loop {
        let Some(_lock) = Lock::try_take(&state_dir().join("worker.lock"), false)? else {
            return Ok(());
        };
        let catalogue = manifest().unwrap_or_else(|_| modules::builtin().clone());
        work(&catalogue, None)?;
        drop(_lock);
        // A job added while the lock was held: take it again.
        let j = load_jobs();
        if j.paused || !j.jobs.iter().any(|x| x.state == "queued") {
            return Ok(());
        }
    }
}

/// Run the queued jobs. `front`: in a terminal (Some(true): may hand off a
/// slow download to the background).
fn work(c: &Catalogue, front: Option<bool>) -> Result<Handoff> {
    loop {
        let next = with_jobs(|j| {
            if j.paused {
                return None;
            }
            j.jobs.iter().find(|x| x.state == "queued" || (front.is_none() && x.active())).cloned()
        })?;
        let Some(job) = next else { return Ok(Handoff::Done) };
        let Some(m) = c.module(&job.id).cloned() else {
            update_job(&job.id, |x| x.state = "cancelled".into())?;
            continue;
        };
        let r = if m.step.as_deref() == Some("stems") { stems_step(&m) } else { install_payloads(c, &m, front) };
        match r {
            Ok(Handoff::Background) => return Ok(Handoff::Background),
            Ok(Handoff::Done) => {
                if update_job(&m.id, |_| {})?.is_some_and(|x| x.state == "cancelled") {
                    continue;
                }
                if m.switch {
                    crate::setup_cli::switch(&[(m.id.as_str(), true)]).ok();
                }
                update_job(&m.id, |x| {
                    x.state = "done".into();
                    x.done = x.total;
                    x.rate = 0;
                })?;
                if front.is_some() {
                    println!("\r✓ {} is ready{}", m.name, " ".repeat(30));
                } else {
                    notify(&format!("{} is ready", m.name));
                }
            }
            Err(_) if update_job(&m.id, |_| {})?.is_some_and(|x| x.state == "cancelled") => {}
            Err(_) if with_jobs(|j| j.paused)? => {
                // Paused: it waits, queued, for `modules resume`.
                update_job(&m.id, |x| {
                    x.state = "queued".into();
                    x.rate = 0;
                })?;
                return Ok(Handoff::Done);
            }
            Err(e) => {
                let tries = update_job(&m.id, |x| x.tries += 1)?.map_or(9, |x| x.tries);
                let retry = tries < 3 && front.is_none();
                update_job(&m.id, |x| {
                    x.state = if retry { "queued" } else { "failed" }.into();
                    x.error = Some(format!("{e:#}"));
                    x.rate = 0;
                })?;
                if retry {
                    std::thread::sleep(Duration::from_secs(30 * tries as u64));
                } else if front.is_some() {
                    bail!("{}: {e:#}", m.name);
                } else {
                    notify(&format!("{} could not be added: {e:#}", m.name));
                }
            }
        }
    }
}

fn notify(text: &str) {
    let _ = Command::new("notify-send")
        .args(["-a", "Patch", "-i", "omarchy-patch", "Patch", text])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

/// Download, check and unpack every payload of `m`, then record its files.
fn install_payloads(c: &Catalogue, m: &Module, front: Option<bool>) -> Result<Handoff> {
    let payloads: Vec<&Payload> = c.payloads_for(&m.id, modules::arch()).collect();
    if payloads.is_empty() {
        bail!("no download for {} on {}", m.name, modules::arch());
    }
    let mut installed = load_installed();
    let mut record = installed.modules.get(&m.id).cloned().unwrap_or_default();
    let mut base = 0;
    for p in &payloads {
        if record.payloads.get(&p.file) == Some(&p.sha256) {
            base += p.size;
            continue;
        }
        let file = match download(m, p, base, front)? {
            Some(f) => f,
            None => return Ok(Handoff::Background),
        };
        update_job(&m.id, |x| x.state = "installing".into())?;
        let files = unpack(m, p, &file)?;
        std::fs::remove_file(&file).ok();
        for f in files {
            if !record.files.contains(&f) {
                record.files.push(f);
            }
        }
        record.payloads.insert(p.file.clone(), p.sha256.clone());
        record.bytes += p.size;
        base += p.size;
    }
    record.version = c.version.clone();
    installed = load_installed();
    installed.modules.insert(m.id.clone(), record);
    save_installed(&installed)?;
    Ok(Handoff::Done)
}

/// The payload, whole and checked, in the download cache; None when it
/// moves to the background (`front == Some(true)` and the line is slow).
fn download(m: &Module, p: &Payload, base: u64, front: Option<bool>) -> Result<Option<PathBuf>> {
    let dir = cache_dir();
    std::fs::create_dir_all(&dir)?;
    let file = dir.join(&p.file);
    let part = dir.join(format!("{}.part", p.file));
    if file.exists() && sha256(&file)? == p.sha256 {
        return Ok(Some(file));
    }
    if dir_size(&part) > p.size {
        std::fs::remove_file(&part).ok();
    }
    if dir_size(&part) < p.size {
        update_job(&m.id, |x| x.state = "downloading".into())?;
        use std::os::unix::process::CommandExt;
        let mut curl = Command::new("curl");
        // SAFETY: only prctl between fork and exec (this is the worker or
        // the CLI, never the engine): curl ends with its worker, so a
        // killed worker leaves no second writer on the part file.
        unsafe {
            curl.pre_exec(|| {
                libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
                Ok(())
            });
        }
        let mut child = curl
            .args(["-fsSL", "--retry", "8", "--retry-delay", "5", "--retry-all-errors", "--connect-timeout", "20", "-C", "-", "-o"])
            .arg(&part)
            .arg(&p.url)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .context("curl")?;
        let start = Instant::now();
        let first = dir_size(&part);
        let mut last_write = Instant::now() - Duration::from_secs(5);
        let status = loop {
            if let Some(st) = child.try_wait()? {
                break st;
            }
            std::thread::sleep(Duration::from_millis(250));
            let now = dir_size(&part);
            let secs = start.elapsed().as_secs_f64().max(0.001);
            let rate = ((now.saturating_sub(first)) as f64 / secs) as u64;
            if last_write.elapsed() >= Duration::from_secs(1) {
                last_write = Instant::now();
                let job = update_job(&m.id, |x| {
                    x.done = base + now;
                    x.rate = rate;
                })?;
                let stop = with_jobs(|j| j.paused)? || job.as_ref().is_none_or(|x| x.state == "cancelled");
                if stop {
                    child.kill().ok();
                    child.wait().ok();
                    bail!("stopped");
                }
                if let Some(job) = &job
                    && front.is_some()
                {
                    print!("\r  {:<22} {}   ", m.name, progress_line(job));
                    std::io::stdout().flush().ok();
                }
                // A slow line: hand over to the background worker (the
                // part file stays; it resumes there).
                if front == Some(true) && start.elapsed() > Duration::from_secs(4) && rate > 0 {
                    let left = Duration::from_secs(p.size.saturating_sub(now) / rate);
                    if left > PATIENCE {
                        child.kill().ok();
                        child.wait().ok();
                        update_job(&m.id, |x| x.state = "queued".into())?;
                        return Ok(None);
                    }
                }
            }
        };
        if !status.success() && dir_size(&part) != p.size {
            let mut err = String::new();
            if let Some(mut e) = child.stderr.take() {
                e.read_to_string(&mut err).ok();
            }
            bail!("download failed: {}", err.trim().lines().last().unwrap_or("curl gave up"));
        }
    }
    update_job(&m.id, |x| {
        x.state = "verifying".into();
        x.done = base + p.size;
        x.rate = 0;
    })?;
    if dir_size(&part) != p.size || sha256(&part)? != p.sha256 {
        std::fs::remove_file(&part).ok();
        bail!("the download was damaged (checksum); it starts again next time");
    }
    std::fs::rename(&part, &file)?;
    Ok(Some(file))
}

fn sha256(path: &Path) -> Result<String> {
    use sha2::Digest;
    let mut f = std::fs::File::open(path)?;
    let mut h = sha2::Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(hex(&h.finalize()))
}

/// Unpack into a staging folder beside the destination, then move each
/// file into place (whole files only, never a half-written one).
fn unpack(m: &Module, p: &Payload, archive: &Path) -> Result<Vec<PathBuf>> {
    let root = dest_root(&p.dest, &m.id);
    let staging = data_dir().join("modules/.staging").join(&m.id);
    std::fs::remove_dir_all(&staging).ok();
    std::fs::create_dir_all(&staging)?;
    let ok = Command::new("tar")
        .args(["--no-same-owner", "--no-same-permissions", "-xf"])
        .arg(archive)
        .arg("-C")
        .arg(&staging)
        .status()
        .context("tar")?
        .success();
    if !ok {
        bail!("{} could not be unpacked", p.file);
    }
    let mut out = Vec::new();
    move_tree(&staging, &staging, &root, &mut out)?;
    std::fs::remove_dir_all(&staging).ok();
    Ok(out)
}

fn move_tree(top: &Path, dir: &Path, root: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    for e in std::fs::read_dir(dir)?.flatten() {
        let path = e.path();
        let kind = e.file_type()?;
        if kind.is_dir() {
            move_tree(top, &path, root, out)?;
        } else if kind.is_file() {
            let rel = path.strip_prefix(top)?;
            let to = root.join(rel);
            if let Some(d) = to.parent() {
                std::fs::create_dir_all(d)?;
            }
            std::fs::rename(&path, &to).or_else(|_| std::fs::copy(&path, &to).map(|_| ()))?;
            out.push(to);
        }
    }
    Ok(())
}

/// The stem separator: the engine prepares it (runtime and model); the
/// job follows its progress.
fn stems_step(m: &Module) -> Result<Handoff> {
    update_job(&m.id, |x| {
        x.state = "installing".into();
        x.total = 100;
    })?;
    let exe = std::env::current_exe()?;
    patch_engine::client::ensure_running(&exe).map_err(anyhow::Error::msg)?;
    let call = |action| {
        patch_engine::client::call(&serde_json::json!({"op":"stems","deck":"a","action":action})).map_err(anyhow::Error::msg)
    };
    if call("status")?["backend_ready"] == true {
        return Ok(Handoff::Done);
    }
    call("setup")?;
    loop {
        std::thread::sleep(Duration::from_secs(2));
        let st = call("status")?;
        if st["backend_ready"] == true {
            return Ok(Handoff::Done);
        }
        if st["setup"]["state"] == "failed" {
            bail!("{}", st["setup"]["error"].as_str().unwrap_or("see the stem setup log"));
        }
        let pct = st["setup"]["progress"].as_u64().unwrap_or(0);
        if update_job(&m.id, |x| x.done = pct)?.is_none_or(|x| x.state == "cancelled") {
            bail!("stopped");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_mix_brings_the_stems_and_stems_stay_free() {
        let c = modules::builtin();
        let all = with_brought(c, c.with_needs(&["ai_dj"]));
        assert!(all.iter().any(|m| m == "stems"), "Auto Mix brings its stems: {all:?}");
        assert!(known(c, &["stems"]).is_ok(), "`modules add stems` works, by name");
        assert!(known(c, &["ai_dj"]).is_ok());
        assert!(!c.needed_by("ai_dj").iter().any(|m| m == "stems"), "removing Auto Mix leaves the stems");
    }

    #[test]
    fn signatures_and_hashes() {
        // A throwaway key (not the release's).
        let kp = ed25519_compact::KeyPair::from_seed(ed25519_compact::Seed::new([7; 32]));
        let body = br#"{"schema":1}"#;
        let sig = kp.sk.sign(body, None);
        let pk = hex(kp.pk.as_ref());
        verify(body, &hex(sig.as_ref()), &pk).unwrap();
        assert!(verify(b"{\"schema\":2}", &hex(sig.as_ref()), &pk).is_err(), "a changed manifest fails");
        assert!(verify(body, &hex(sig.as_ref()), PUBLIC_KEY).is_err(), "another key fails");
        assert_eq!(base64("aGVsbG8=").unwrap(), b"hello");
        assert_eq!(unhex(&hex(&[0, 255, 16])).unwrap(), [0, 255, 16]);
        assert_eq!(eta(Duration::from_secs(3725)), "1:02:05");
    }
}
