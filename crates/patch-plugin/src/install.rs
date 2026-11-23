//! Installing a VST3 plugin the user downloaded: a `.zip`, a `.tar.gz` or a
//! bundle folder, into `~/.vst3`.
//!
//! The download is untrusted. Nothing in it runs here: no shell, no
//! outside unpacker, and the plugin's code is not loaded (Add does that
//! later, in a probe process of its own). Only the `.vst3` bundles inside
//! are unpacked, every entry a plain file or folder under its bundle, within
//! fixed limits, into a private staging folder; a bundle must hold a Linux
//! binary for this machine (read from its ELF header) before it moves into
//! place in one rename.

use anyhow::{Context, Result, bail};
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};

/// Everything unpacked from one download, together.
const MAX_BYTES: u64 = 2 << 30;
/// Entries in one download, unpacked or not.
const MAX_ENTRIES: usize = 20_000;
/// Bundles in one download.
const MAX_BUNDLES: usize = 8;
/// Folders deep inside a bundle.
const MAX_DEPTH: usize = 16;
/// Bytes in one file or folder name.
const MAX_NAME: usize = 255;
/// Characters in a bundle's name.
const MAX_BUNDLE_NAME: usize = 128;

/// A bundle now in the plugins folder.
#[derive(Debug, Clone, PartialEq)]
pub struct Installed {
    pub name: String,
    pub path: PathBuf,
    /// It took the place of one with the same name.
    pub replaced: bool,
}

/// What an install did, in a line.
pub fn summary(done: &[Installed]) -> String {
    let names = |replaced: bool| -> Vec<&str> {
        done.iter().filter(|d| d.replaced == replaced).map(|d| d.name.as_str()).collect()
    };
    let mut parts = Vec::new();
    if !names(false).is_empty() {
        parts.push(format!("Installed {}", names(false).join(", ")));
    }
    if !names(true).is_empty() {
        parts.push(format!("Updated {}", names(true).join(", ")));
    }
    format!("{}: Add checks it and puts it in the rack", parts.join(" · "))
}

/// Where downloaded plugins go: `~/.vst3`.
pub fn plugins_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".vst3"))
}

/// The desktop's Trash: `$XDG_DATA_HOME/Trash`, else `~/.local/share/Trash`.
pub fn trash_dir() -> Option<PathBuf> {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share")))
        .map(|data| data.join("Trash"))
}

/// Uninstall a bundle installed into `from` (`~/.vst3`): it moves to the
/// Trash (`trash`, the freedesktop layout, so a file manager restores it),
/// in one rename. A bundle anywhere else (the system's) is not touched.
/// Answers its name.
pub fn uninstall(bundle: &Path, from: &Path, trash: &Path) -> Result<String> {
    let name = bundle.file_name().and_then(|n| n.to_str()).context("no bundle name")?.to_string();
    if !name.to_ascii_lowercase().ends_with(".vst3") {
        bail!("{} is not a .vst3 plugin", display_name(bundle));
    }
    let from = from.canonicalize().with_context(|| format!("open {}", from.display()))?;
    // The bundle itself (a link moves, not what it points to) in `from`.
    let parent = bundle.parent().and_then(|p| p.canonicalize().ok());
    if parent.as_deref() != Some(from.as_path()) {
        bail!("{} is not one of yours (only plugins in {} can be uninstalled)", display_name(bundle), from.display());
    }
    let at = from.join(&name);
    fs::symlink_metadata(&at).with_context(|| format!("read {}", at.display()))?;
    let (files, info) = (trash.join("files"), trash.join("info"));
    for dir in [&files, &info] {
        fs::DirBuilder::new().recursive(true).mode(0o700).create(dir).with_context(|| format!("create {}", dir.display()))?;
    }
    // A name the Trash does not hold yet, claimed by its info file.
    let stem = &name[..name.len() - 5];
    let mut taken = None;
    for n in 0..1000 {
        let entry = if n == 0 { name.clone() } else { format!("{stem}.{n}.vst3") };
        match fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(info.join(format!("{entry}.trashinfo"))) {
            Ok(file) => {
                taken = Some((entry, file));
                break;
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e).with_context(|| format!("write in {}", info.display())),
        }
    }
    let (entry, mut file) = taken.context("the Trash has too many plugins of that name")?;
    let record = format!("[Trash Info]\nPath={}\nDeletionDate={}\n", uri_path(&at), local_now());
    let moved = file
        .write_all(record.as_bytes())
        .and_then(|()| file.sync_all())
        .and_then(|()| fs::rename(&at, files.join(&entry)));
    if let Err(e) = moved {
        let _ = fs::remove_file(info.join(format!("{entry}.trashinfo")));
        return Err(e).with_context(|| format!("move {name} to the Trash"));
    }
    let shown = display_name(&at);
    Ok(shown.strip_suffix(".vst3").unwrap_or(&shown).to_string())
}

/// Uninstall one of the user's plugins (Installed list, `plugins
/// uninstall`), never one in the rack: the chain would lose it live.
pub fn uninstall_unused(bundle: &Path, rack: &crate::RackConfig) -> Result<String> {
    if let Some(slot) = rack.slots.iter().find(|s| s.plugin == bundle) {
        bail!("{} is in the rack (slot {}): remove it there first", display_name(bundle), slot.id);
    }
    let from = plugins_dir().context("no home folder")?;
    uninstall(bundle, &from, &trash_dir().context("no Trash folder")?)
}

/// A path as the Trash spec writes it: each byte outside the unreserved
/// set (and `/`) percent-encoded.
fn uri_path(path: &Path) -> String {
    use std::os::unix::ffi::OsStrExt as _;
    path.as_os_str()
        .as_bytes()
        .iter()
        .map(|&b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// Now, local time, as `2026-10-23T01:42:18`.
fn local_now() -> String {
    // SAFETY: time and localtime_r write only into the values given.
    let tm = unsafe {
        let now = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&now, &mut tm);
        tm
    };
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}",
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min,
        tm.tm_sec
    )
}

/// Install the bundles in `source` (an archive, a bundle folder, or a file
/// inside one) into `into`.
pub fn install(source: &Path, into: &Path) -> Result<Vec<Installed>> {
    // The file picked may be a link; links inside a bundle are refused.
    let meta = fs::metadata(source).with_context(|| format!("read {}", source.display()))?;
    fs::DirBuilder::new().recursive(true).mode(0o755).create(into).with_context(|| format!("create {}", into.display()))?;
    let into = into.canonicalize().with_context(|| format!("open {}", into.display()))?;
    let mut stage = Stage::new(&into)?;
    let mut budget = Budget::default();
    if let Some(bundle) = enclosing_bundle(source, &meta) {
        if bundle.canonicalize().ok().and_then(|b| b.parent().map(Path::to_path_buf)) == Some(into.clone()) {
            bail!("{} is installed already", display_name(&bundle));
        }
        let name = bundle_name(&bundle)?;
        copy_folder(&bundle, &stage.bundle(0, &name), 0, &mut budget)?;
    } else if meta.is_file() {
        let mut file = File::open(source).with_context(|| format!("open {}", source.display()))?;
        let mut magic = [0u8; 262];
        let n = read_up_to(&mut file, &mut magic)?;
        drop(file);
        let file = File::open(source)?;
        match kind(&magic[..n]) {
            Some(Kind::Zip) => unzip(file, &mut stage, &mut budget)?,
            Some(Kind::Gzip) => untar(flate2::read::GzDecoder::new(file), &mut stage, &mut budget)?,
            Some(Kind::Tar) => untar(file, &mut stage, &mut budget)?,
            None => bail!("{} is not a .zip, .tar.gz or .vst3 plugin", display_name(source)),
        }
    } else {
        bail!("{} is not a file or a .vst3 folder", display_name(source));
    }
    let bundles = stage.bundles()?;
    if bundles.is_empty() {
        bail!("{} holds no VST3 plugin (no .vst3 folder)", display_name(source));
    }
    let mut ready = Vec::new();
    let mut refused = Vec::new();
    for (name, path) in bundles {
        match native_binary(&path) {
            Ok(()) => ready.push((name, path)),
            Err(e) => refused.push(format!("{name}: {e}")),
        }
    }
    ready.sort_by(|a, b| a.0.cmp(&b.0));
    if let Some(w) = ready.windows(2).find(|w| w[0].0 == w[1].0) {
        bail!("the download holds two different bundles named {}", w[0].0);
    }
    if ready.is_empty() {
        bail!("{}", refused.join("; "));
    }
    let mut done = Vec::new();
    for (name, staged) in ready {
        let path = into.join(&name);
        let replaced = place(&staged, &path)?;
        done.push(Installed { name: name.trim_end_matches(".vst3").to_string(), path, replaced });
    }
    Ok(done)
}

enum Kind {
    Zip,
    Gzip,
    Tar,
}

/// From the file's first bytes, not its name.
fn kind(magic: &[u8]) -> Option<Kind> {
    if magic.starts_with(b"PK\x03\x04") || magic.starts_with(b"PK\x05\x06") {
        Some(Kind::Zip)
    } else if magic.starts_with(&[0x1f, 0x8b]) {
        Some(Kind::Gzip)
    } else if magic.get(257..262) == Some(b"ustar") {
        Some(Kind::Tar)
    } else {
        None
    }
}

fn read_up_to(file: &mut File, buf: &mut [u8]) -> io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match file.read(&mut buf[n..])? {
            0 => break,
            k => n += k,
        }
    }
    Ok(n)
}

/// The bundle folder picked, or the one around the file picked inside it.
fn enclosing_bundle(source: &Path, meta: &fs::Metadata) -> Option<PathBuf> {
    let is_bundle = |p: &Path| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("vst3"));
    if meta.is_dir() && is_bundle(source) {
        return Some(source.to_path_buf());
    }
    source.ancestors().skip(1).take(MAX_DEPTH).find(|p| is_bundle(p) && p.is_dir()).map(Path::to_path_buf)
}

/// A name fit to show: no control characters, not too long.
fn display_name(path: &Path) -> String {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let clean: String = name.chars().filter(|c| !c.is_control()).take(80).collect();
    if clean.is_empty() { "The file".to_string() } else { clean }
}

/// A bundle's folder name, checked: `Name.vst3`.
fn bundle_name(path: &Path) -> Result<String> {
    let name = path.file_name().and_then(|n| n.to_str()).context("a bundle's name is not text")?;
    check_bundle_name(name)?;
    Ok(name.to_string())
}

fn check_bundle_name(name: &str) -> Result<()> {
    let stem = name.len().checked_sub(5).and_then(|i| name.get(..i)).filter(|_| name.to_ascii_lowercase().ends_with(".vst3"));
    let Some(stem) = stem else { bail!("{name:?} is not a .vst3 bundle") };
    if stem.trim().is_empty()
        || stem.starts_with('.')
        || stem.chars().count() > MAX_BUNDLE_NAME
        || stem.chars().any(|c| c.is_control() || c == '/' || c == '\\')
    {
        bail!("a bundle has a name Patch will not install");
    }
    Ok(())
}

/// What one download may unpack.
#[derive(Default)]
struct Budget {
    bytes: u64,
    entries: usize,
}

impl Budget {
    fn entry(&mut self) -> Result<()> {
        self.entries += 1;
        if self.entries > MAX_ENTRIES {
            bail!("the download has more than {MAX_ENTRIES} files");
        }
        Ok(())
    }

    /// Copy at most what is left; more is refused, whatever the archive
    /// said the size was.
    fn copy(&mut self, from: &mut dyn Read, to: &mut File) -> Result<()> {
        let left = MAX_BYTES - self.bytes;
        let n = io::copy(&mut from.take(left + 1), to)?;
        if n > left {
            bail!("the download unpacks to more than {} GB", MAX_BYTES >> 30);
        }
        self.bytes += n;
        Ok(())
    }
}

/// A private folder beside the plugins folder (the same disk, so a bundle
/// moves into place in one rename), removed when done.
struct Stage {
    dir: PathBuf,
    /// The bundle root inside the download (e.g. `linux/X.vst3`) → its slot.
    roots: Vec<PathBuf>,
}

impl Stage {
    fn new(into: &Path) -> Result<Self> {
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.subsec_nanos());
        let dir = into.join(format!(".patch-install-{}-{nanos}", std::process::id()));
        fs::DirBuilder::new().mode(0o700).create(&dir).with_context(|| format!("create {}", dir.display()))?;
        Ok(Self { dir, roots: Vec::new() })
    }

    fn bundle(&self, slot: usize, name: &str) -> PathBuf {
        self.dir.join(slot.to_string()).join(name)
    }

    /// Where an archive entry goes: under its bundle's slot, or None for an
    /// entry outside any bundle (a readme, another format), not unpacked.
    fn target(&mut self, entry: &Path) -> Result<Option<(PathBuf, usize)>> {
        let parts = clean_parts(entry)?;
        let Some(at) = parts.iter().position(|p| p.to_ascii_lowercase().ends_with(".vst3")) else {
            return Ok(None);
        };
        check_bundle_name(&parts[at])?;
        let root: PathBuf = parts[..=at].iter().collect();
        let slot = match self.roots.iter().position(|r| *r == root) {
            Some(slot) => slot,
            None => {
                if self.roots.len() >= MAX_BUNDLES {
                    bail!("the download holds more than {MAX_BUNDLES} plugins");
                }
                self.roots.push(root);
                self.roots.len() - 1
            }
        };
        let inner = &parts[at + 1..];
        if inner.len() > MAX_DEPTH {
            bail!("the download nests folders too deep");
        }
        let mut path = self.bundle(slot, &parts[at]);
        path.extend(inner);
        Ok(Some((path, inner.len())))
    }

    /// The bundles unpacked, by name.
    fn bundles(&self) -> Result<Vec<(String, PathBuf)>> {
        let mut found = Vec::new();
        for slot in 0..self.roots.len().max(1) {
            let Ok(entries) = fs::read_dir(self.dir.join(slot.to_string())) else { continue };
            for entry in entries {
                let entry = entry?;
                found.push((entry.file_name().to_string_lossy().into_owned(), entry.path()));
            }
        }
        Ok(found)
    }
}

impl Drop for Stage {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

/// An archive path as plain names: no root, no `..`, no `.`, nothing a
/// file system would read as anything but a name.
fn clean_parts(path: &Path) -> Result<Vec<String>> {
    let mut parts = Vec::new();
    for c in path.components() {
        match c {
            Component::Normal(name) => {
                let name = name.to_str().context("a file in the download has a name that is not text")?;
                if name.len() > MAX_NAME || name.chars().any(|c| c.is_control() || c == '\\') {
                    bail!("a file in the download has a name Patch will not unpack");
                }
                parts.push(name.to_string());
            }
            Component::CurDir => {}
            _ => bail!("the download has a file outside its own folder ({})", path.display()),
        }
    }
    Ok(parts)
}

fn make_dirs(path: &Path) -> Result<()> {
    fs::DirBuilder::new().recursive(true).mode(0o755).create(path)?;
    Ok(())
}

/// A new file, never one already there, never through a link.
fn new_file(path: &Path) -> Result<File> {
    if let Some(parent) = path.parent() {
        make_dirs(parent)?;
    }
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o644)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .with_context(|| format!("the download repeats or cannot unpack {}", display_name(path)))
}

fn unzip(file: File, stage: &mut Stage, budget: &mut Budget) -> Result<()> {
    let mut zip = zip::ZipArchive::new(io::BufReader::new(file)).context("not a readable .zip")?;
    for i in 0..zip.len() {
        budget.entry()?;
        let mut entry = zip.by_index(i).context("not a readable .zip")?;
        // The raw name, checked here rather than trusting a cleaned one.
        let raw = PathBuf::from(entry.name().context("a file in the download has an unreadable name")?.into_owned());
        let Some((path, _)) = stage.target(&raw)? else { continue };
        let kind = entry.unix_mode().map(|m| m & 0o170_000);
        if entry.is_symlink() || !matches!(kind, None | Some(0) | Some(0o100_000) | Some(0o040_000)) {
            bail!("the plugin contains a link or special file ({}), which Patch does not install", display_name(&raw));
        }
        if entry.encrypted() {
            bail!("the download is encrypted");
        }
        if entry.is_dir() {
            make_dirs(&path)?;
        } else {
            let mut out = new_file(&path)?;
            budget.copy(&mut entry, &mut out)?;
            out.flush()?;
        }
    }
    Ok(())
}

fn untar(reader: impl Read, stage: &mut Stage, budget: &mut Budget) -> Result<()> {
    let mut tar = tar::Archive::new(reader);
    for entry in tar.entries().context("not a readable archive")? {
        budget.entry()?;
        let mut entry = entry.context("not a readable archive")?;
        let raw = entry.path().context("a file in the download has an unreadable name")?.into_owned();
        let Some((path, _)) = stage.target(&raw)? else { continue };
        match entry.header().entry_type() {
            tar::EntryType::Directory => make_dirs(&path)?,
            tar::EntryType::Regular | tar::EntryType::Continuous => {
                let mut out = new_file(&path)?;
                budget.copy(&mut entry, &mut out)?;
                out.flush()?;
            }
            // Extended headers carry names and times for the next entry.
            tar::EntryType::XHeader | tar::EntryType::XGlobalHeader | tar::EntryType::GNULongName => {}
            _ => bail!("the plugin contains a link or special file ({}), which Patch does not install", display_name(&raw)),
        }
    }
    Ok(())
}

/// A bundle folder on disk, copied entry by entry under the same rules.
fn copy_folder(from: &Path, to: &Path, depth: usize, budget: &mut Budget) -> Result<()> {
    if depth > MAX_DEPTH {
        bail!("the plugin nests folders too deep");
    }
    make_dirs(to)?;
    for entry in fs::read_dir(from).with_context(|| format!("read {}", from.display()))? {
        budget.entry()?;
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_str().context("a file in the plugin has a name that is not text")?;
        clean_parts(Path::new(name))?;
        // Not followed: a link is refused, wherever it points.
        let kind = entry.file_type()?;
        if kind.is_dir() {
            copy_folder(&entry.path(), &to.join(name), depth + 1, budget)?;
        } else if kind.is_file() {
            let mut input = fs::OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW).open(entry.path())?;
            let mut out = new_file(&to.join(name))?;
            budget.copy(&mut input, &mut out)?;
        } else {
            bail!("the plugin contains a link or special file ({name}), which Patch does not install");
        }
    }
    Ok(())
}

/// The bundle holds a shared library built for this machine; its header
/// says so without loading it.
fn native_binary(bundle: &Path) -> Result<()> {
    let arch = crate::scan::arch_dir();
    let dir = bundle.join("Contents").join(arch);
    let libraries: Vec<PathBuf> = fs::read_dir(&dir)
        .map(|d| d.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "so")).collect())
        .unwrap_or_default();
    if libraries.is_empty() {
        bail!("no Linux build for this computer (Contents/{arch}): a Windows or macOS download?");
    }
    for library in &libraries {
        let mut header = [0u8; 20];
        let n = read_up_to(&mut File::open(library)?, &mut header)?;
        if !elf_for_this_machine(&header[..n]) {
            bail!("{} is not a Linux library for this computer", display_name(library));
        }
    }
    Ok(())
}

/// An ELF shared object, 64-bit little-endian, for this processor.
fn elf_for_this_machine(header: &[u8]) -> bool {
    const ET_DYN: u16 = 3;
    let machine: u16 = if cfg!(target_arch = "aarch64") { 183 } else { 62 };
    header.len() >= 20
        && header.starts_with(b"\x7fELF")
        && header[4] == 2
        && header[5] == 1
        && u16::from_le_bytes([header[16], header[17]]) == ET_DYN
        && u16::from_le_bytes([header[18], header[19]]) == machine
}

/// Into place in one rename; one already there is swapped out whole, then
/// removed. True when it replaced one.
fn place(staged: &Path, path: &Path) -> Result<bool> {
    for dir in walk_dirs(staged)? {
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o755))?;
    }
    match fs::symlink_metadata(path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            rename(staged, path, libc::RENAME_NOREPLACE)?;
            Ok(false)
        }
        Ok(meta) if meta.is_dir() => {
            // The old one ends up where the new one was staged, and goes
            // with the stage.
            rename(staged, path, libc::RENAME_EXCHANGE)?;
            Ok(true)
        }
        Ok(_) => bail!("{} is in the way and is not a plugin folder: remove it first", path.display()),
        Err(e) => Err(e.into()),
    }
}

fn walk_dirs(root: &Path) -> Result<Vec<PathBuf>> {
    let mut dirs = vec![root.to_path_buf()];
    let mut i = 0;
    while i < dirs.len() {
        for entry in fs::read_dir(&dirs[i])?.flatten() {
            if entry.file_type()?.is_dir() {
                dirs.push(entry.path());
            }
        }
        i += 1;
    }
    Ok(dirs)
}

fn rename(from: &Path, to: &Path, flags: libc::c_uint) -> Result<()> {
    use std::os::unix::ffi::OsStrExt;
    let from_c = std::ffi::CString::new(from.as_os_str().as_bytes())?;
    let to_c = std::ffi::CString::new(to.as_os_str().as_bytes())?;
    // SAFETY: both are NUL-terminated paths that outlive the call.
    let r = unsafe { libc::renameat2(libc::AT_FDCWD, from_c.as_ptr(), libc::AT_FDCWD, to_c.as_ptr(), flags) };
    if r != 0 {
        return Err(io::Error::last_os_error()).with_context(|| format!("move the plugin to {}", to.display()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A temporary folder of its own, removed after.
    struct Temp(PathBuf);
    impl Temp {
        fn new(tag: &str) -> Self {
            let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
            let dir = std::env::temp_dir().join(format!("patch-install-{tag}-{}-{nanos}", std::process::id()));
            fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// The first bytes of a shared library for this machine.
    fn elf() -> Vec<u8> {
        let machine: u16 = if cfg!(target_arch = "aarch64") { 183 } else { 62 };
        let mut h = b"\x7fELF\x02\x01\x01\x00\x00\x00\x00\x00\x00\x00\x00\x00".to_vec();
        h.extend_from_slice(&3u16.to_le_bytes());
        h.extend_from_slice(&machine.to_le_bytes());
        h.extend_from_slice(&[0u8; 44]);
        h
    }

    fn arch() -> &'static str {
        crate::scan::arch_dir()
    }

    enum E<'a> {
        File(&'a str, Vec<u8>),
        Dir(&'a str),
        Link(&'a str, &'a str),
    }

    fn zip_of(path: &Path, entries: &[E]) {
        use zip::write::SimpleFileOptions;
        let mut z = zip::ZipWriter::new(File::create(path).unwrap());
        for e in entries {
            match e {
                E::File(name, data) => {
                    z.start_file(*name, SimpleFileOptions::default()).unwrap();
                    z.write_all(data).unwrap();
                }
                E::Dir(name) => z.add_directory(*name, SimpleFileOptions::default()).unwrap(),
                E::Link(name, target) => z.add_symlink(*name, *target, SimpleFileOptions::default()).unwrap(),
            }
        }
        z.finish().unwrap();
    }

    /// Names written raw, as a hostile archive would (the builder itself
    /// refuses `..` and roots).
    fn tar_gz_of(path: &Path, entries: &[E]) {
        let gz = flate2::write::GzEncoder::new(File::create(path).unwrap(), flate2::Compression::fast());
        let mut t = tar::Builder::new(gz);
        for e in entries {
            let mut h = tar::Header::new_gnu();
            let (name, data, kind, mode): (&str, &[u8], _, _) = match e {
                E::File(name, data) => (name, data, tar::EntryType::Regular, 0o644),
                E::Dir(name) => (name, &[], tar::EntryType::Directory, 0o755),
                E::Link(name, target) => {
                    h.set_link_name(target).unwrap();
                    (name, &[], tar::EntryType::Symlink, 0o777)
                }
            };
            h.as_gnu_mut().unwrap().name[..name.len()].copy_from_slice(name.as_bytes());
            h.set_size(data.len() as u64);
            h.set_mode(mode);
            h.set_entry_type(kind);
            h.set_cksum();
            t.append(&h, data).unwrap();
        }
        t.into_inner().unwrap().finish().unwrap();
    }

    fn good(prefix: &str, name: &str) -> Vec<E<'static>> {
        let so = Box::leak(format!("{prefix}{name}.vst3/Contents/{}/{name}.so", arch()).into_boxed_str());
        let info = Box::leak(format!("{prefix}{name}.vst3/Contents/Resources/moduleinfo.json").into_boxed_str());
        vec![E::Dir(Box::leak(format!("{prefix}{name}.vst3/Contents/").into_boxed_str())), E::File(so, elf()), E::File(info, br#"{"Name":"Test"}"#.to_vec()), E::File("README.txt", b"hi".to_vec())]
    }

    fn leftovers(dir: &Path) -> Vec<String> {
        fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with('.'))
            .collect()
    }

    #[test]
    fn a_zip_installs_only_its_bundle() {
        let t = Temp::new("zip");
        let into = t.0.join("vst3");
        zip_of(&t.0.join("p.zip"), &good("Fifth Order/linux/", "Fifth Order"));
        let done = install(&t.0.join("p.zip"), &into).unwrap();
        assert_eq!(done.len(), 1);
        assert_eq!(done[0].name, "Fifth Order");
        assert!(!done[0].replaced);
        let bundle = into.join("Fifth Order.vst3");
        assert!(bundle.join(format!("Contents/{}/Fifth Order.so", arch())).is_file());
        assert!(!into.join("README.txt").exists(), "only the bundle is unpacked");
        assert!(leftovers(&into).is_empty(), "the stage is gone: {:?}", leftovers(&into));
        assert_eq!(fs::metadata(&bundle).unwrap().permissions().mode() & 0o777, 0o755);
        // It is found like any other.
        assert!(crate::scan::bundle_info(&bundle).native);
    }

    #[test]
    fn a_tar_gz_installs_and_a_second_install_replaces() {
        let t = Temp::new("tgz");
        let into = t.0.join("vst3");
        tar_gz_of(&t.0.join("p.tar.gz"), &good("", "Ripper"));
        assert!(!install(&t.0.join("p.tar.gz"), &into).unwrap()[0].replaced);
        fs::write(into.join("Ripper.vst3/old-only"), b"x").unwrap();
        let again = install(&t.0.join("p.tar.gz"), &into).unwrap();
        assert!(again[0].replaced);
        assert!(!into.join("Ripper.vst3/old-only").exists(), "the old bundle is gone whole");
        assert!(leftovers(&into).is_empty());
    }

    #[test]
    fn a_path_out_of_the_archive_is_refused() {
        for name in ["../escape.vst3/Contents/x.so", "/abs/X.vst3/Contents/x.so", "a/../../X.vst3/y"] {
            let t = Temp::new("slip");
            let into = t.0.join("vst3");
            tar_gz_of(&t.0.join("p.tgz"), &[E::File(name, elf())]);
            let e = install(&t.0.join("p.tgz"), &into).unwrap_err().to_string();
            assert!(e.contains("outside") || e.contains("unreadable"), "{name}: {e}");
            assert!(!t.0.join("escape.vst3").exists());
            assert!(leftovers(&into).is_empty());
        }
        // A zip with ../ in a raw name.
        let t = Temp::new("zipslip");
        let into = t.0.join("vst3");
        zip_of(&t.0.join("p.zip"), &[E::File("X.vst3/../../evil.so", elf())]);
        assert!(install(&t.0.join("p.zip"), &into).unwrap_err().to_string().contains("outside"));
        assert!(!t.0.join("evil.so").exists());
    }

    #[test]
    fn links_inside_a_bundle_are_refused() {
        let t = Temp::new("link");
        let into = t.0.join("vst3");
        let mut zip = good("", "Linky");
        zip.push(E::Link("Linky.vst3/Contents/Resources/passwd", "/etc/passwd"));
        zip_of(&t.0.join("p.zip"), &zip);
        assert!(install(&t.0.join("p.zip"), &into).unwrap_err().to_string().contains("link"));
        let mut tgz = good("", "Linky");
        tgz.push(E::Link("Linky.vst3/Contents/x", "/etc"));
        tar_gz_of(&t.0.join("p.tgz"), &tgz);
        assert!(install(&t.0.join("p.tgz"), &into).unwrap_err().to_string().contains("link"));
        assert!(!into.join("Linky.vst3").exists());
        assert!(leftovers(&into).is_empty());
    }

    #[test]
    fn a_bundle_without_a_build_for_this_machine_is_refused() {
        let t = Temp::new("arch");
        let into = t.0.join("vst3");
        zip_of(&t.0.join("win.zip"), &[E::File("Win.vst3/Contents/x86_64-win/Win.vst3", b"MZ".to_vec())]);
        let e = install(&t.0.join("win.zip"), &into).unwrap_err().to_string();
        assert!(e.contains("no Linux build"), "{e}");
        let wrong = Box::leak(format!("Fake.vst3/Contents/{}/Fake.so", arch()).into_boxed_str());
        zip_of(&t.0.join("fake.zip"), &[E::File(wrong, b"#!/bin/sh\nrm -rf ~\n".to_vec())]);
        let e = install(&t.0.join("fake.zip"), &into).unwrap_err().to_string();
        assert!(e.contains("not a Linux library"), "{e}");
        assert!(!into.join("Win.vst3").exists() && !into.join("Fake.vst3").exists());
    }

    #[test]
    fn too_much_to_unpack_is_refused() {
        let t = Temp::new("bomb");
        let into = t.0.join("vst3");
        let mut budget = Budget { bytes: MAX_BYTES - 10, entries: 0 };
        let mut stage = Stage::new(&into_dir(&into)).unwrap();
        tar_gz_of(&t.0.join("p.tgz"), &[E::File("Big.vst3/Contents/blob", vec![0u8; 64])]);
        let gz = flate2::read::GzDecoder::new(File::open(t.0.join("p.tgz")).unwrap());
        let e = untar(gz, &mut stage, &mut budget).unwrap_err().to_string();
        assert!(e.contains("more than"), "{e}");
        let mut many = Budget { bytes: 0, entries: MAX_ENTRIES };
        assert!(many.entry().is_err());
    }

    fn into_dir(into: &Path) -> PathBuf {
        fs::create_dir_all(into).unwrap();
        into.to_path_buf()
    }

    #[test]
    fn not_an_archive_is_refused_by_its_contents() {
        let t = Temp::new("kind");
        let into = t.0.join("vst3");
        fs::write(t.0.join("plugin.zip"), b"<html>not a zip</html>").unwrap();
        let e = install(&t.0.join("plugin.zip"), &into).unwrap_err().to_string();
        assert!(e.contains("is not a .zip"), "{e}");
        zip_of(&t.0.join("none.zip"), &[E::File("readme.txt", b"x".to_vec())]);
        assert!(install(&t.0.join("none.zip"), &into).unwrap_err().to_string().contains("no VST3"));
    }

    #[test]
    fn a_bundle_folder_or_a_file_inside_one_installs() {
        let t = Temp::new("folder");
        let into = t.0.join("vst3");
        let src = t.0.join("Downloads/Folder Plug.vst3");
        let so = src.join(format!("Contents/{}/Folder Plug.so", arch()));
        fs::create_dir_all(so.parent().unwrap()).unwrap();
        fs::write(&so, elf()).unwrap();
        // Picked by its library, inside the bundle.
        let done = install(&so, &into).unwrap();
        assert_eq!(done[0].name, "Folder Plug");
        assert!(into.join(format!("Folder Plug.vst3/Contents/{}/Folder Plug.so", arch())).is_file());
        // A link in the folder is refused.
        std::os::unix::fs::symlink("/etc/passwd", src.join("Contents/passwd")).unwrap();
        assert!(install(&src, &into).unwrap_err().to_string().contains("link"));
        // The installed one is not installed over itself.
        assert!(install(&into.join("Folder Plug.vst3"), &into).unwrap_err().to_string().contains("installed already"));
    }

    #[test]
    fn names_that_are_not_plain_are_refused() {
        for name in [".hidden.vst3", "a\u{1b}[31m.vst3", ".vst3", "x.vst"] {
            assert!(check_bundle_name(name).is_err(), "{name:?}");
        }
        assert!(check_bundle_name("Auburn Sounds Graillon 3.vst3").is_ok());
        assert!(clean_parts(Path::new("a/b\\c")).is_err());
        assert!(clean_parts(Path::new("a/\u{7}/c")).is_err());
    }
    #[test]
    fn uninstall_moves_a_bundle_of_yours_to_the_trash() {
        let t = Temp::new("uninstall");
        let (vst3, trash) = (t.0.join("vst3"), t.0.join("Trash"));
        let bundle = |name: &str| {
            let so = vst3.join(format!("{name}.vst3/Contents/{}/{name}.so", arch()));
            fs::create_dir_all(so.parent().unwrap()).unwrap();
            fs::write(&so, elf()).unwrap();
            vst3.join(format!("{name}.vst3"))
        };
        assert_eq!(uninstall(&bundle("Gone Plug"), &vst3, &trash).unwrap(), "Gone Plug");
        assert!(!vst3.join("Gone Plug.vst3").exists());
        assert!(trash.join("files/Gone Plug.vst3/Contents").is_dir());
        let info = fs::read_to_string(trash.join("info/Gone Plug.vst3.trashinfo")).unwrap();
        assert!(info.starts_with("[Trash Info]\nPath=/"), "{info}");
        assert!(info.contains("/Gone%20Plug.vst3\nDeletionDate=20"), "{info}");
        // The same name again takes a name of its own in the Trash.
        uninstall(&bundle("Gone Plug"), &vst3, &trash).unwrap();
        assert!(trash.join("files/Gone Plug.1.vst3").is_dir());
        assert!(trash.join("info/Gone Plug.1.vst3.trashinfo").is_file());
        // Not one of yours, or not a plugin: left alone.
        let elsewhere = t.0.join("system/Other.vst3");
        fs::create_dir_all(&elsewhere).unwrap();
        assert!(uninstall(&elsewhere, &vst3, &trash).unwrap_err().to_string().contains("not one of yours"));
        assert!(uninstall(&vst3.join("../system/Other.vst3"), &vst3, &trash).is_err());
        assert!(elsewhere.is_dir());
        fs::write(vst3.join("notes.txt"), "x").unwrap();
        assert!(uninstall(&vst3.join("notes.txt"), &vst3, &trash).is_err());
        assert!(vst3.join("notes.txt").is_file());
    }
}
