//! Spawn a command so it dies with this process, without forking the engine.
//!
//! `Command`'s before-exec hook forces `fork()` of the calling process. After
//! a deck has decoded a track that copies the engine's page tables, and the
//! first writes in the child dirty that working set under the audio callback.
//! The volume listener used to do this every time `pactl subscribe` was
//! respawned. The helper below is a few kilobytes: the engine `posix_spawn`s
//! it, and the helper arms `PR_SET_PDEATHSIG` in its own address space, then
//! execs the command. The signal is preserved across exec, so `pactl` still
//! dies when the engine does.

use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::OnceLock;

const HELPER: &[u8] = include_bytes!(env!("PATCH_PDEATH_EXEC"));

/// Write the helper into `dir` (mode 0700) and return its path.
pub(super) fn materialize(dir: &Path) -> io::Result<PathBuf> {
    let path = dir.join("pdeath-exec");
    if std::fs::read(&path).ok().as_deref() == Some(HELPER) {
        let _ = std::fs::set_permissions(&path, executable());
        return Ok(path);
    }
    // A second writer in this process (tests materialize in parallel) must
    // not truncate a file another thread is executing.
    static WRITES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = WRITES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmp = dir.join(format!(".pdeath-exec.{}.{n}.tmp", std::process::id()));
    {
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o700)
            .open(&tmp)?;
        file.write_all(HELPER)?;
        file.sync_all()?;
    }
    std::fs::set_permissions(&tmp, executable())?;
    if let Err(error) = std::fs::rename(&tmp, &path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(error);
    }
    Ok(path)
}

fn executable() -> std::fs::Permissions {
    use std::os::unix::fs::PermissionsExt;
    std::fs::Permissions::from_mode(0o700)
}

fn helper_path() -> io::Result<PathBuf> {
    static CACHED: OnceLock<PathBuf> = OnceLock::new();
    if let Some(path) = CACHED.get() {
        return Ok(path.clone());
    }
    let path = materialize(&patch_core::ipc::runtime_dir())?;
    let _ = CACHED.set(path.clone());
    Ok(path)
}

/// `posix_spawn` the helper, which execs `argv`. No before-exec hook.
pub(super) fn spawn(argv: &[&str]) -> io::Result<Child> {
    spawn_with(&helper_path()?, argv)
}

pub(super) fn spawn_with(helper: &Path, argv: &[&str]) -> io::Result<Child> {
    if argv.is_empty() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "empty command"));
    }
    Command::new(helper)
        .args(argv)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use std::os::unix::fs::PermissionsExt;

    struct RmDir(PathBuf);
    impl Drop for RmDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn scratch() -> (PathBuf, RmDir) {
        static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("patch-pdeath-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let rm = RmDir(dir.clone());
        (dir, rm)
    }

    /// A thread spawning at the moment another test writes its helper hands
    /// that write handle to a child until the child execs, and running the
    /// file meanwhile fails with ETXTBSY. Tests write and spawn side by side
    /// all the time; the engine's listener simply tries again a second later.
    fn spawn_when_free(helper: &Path, argv: &[&str]) -> Child {
        for _ in 0..200 {
            match spawn_with(helper, argv) {
                Err(error) if error.kind() == io::ErrorKind::ExecutableFileBusy => {
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
                other => return other.unwrap(),
            }
        }
        panic!("the helper stayed busy for a second");
    }

    fn shell_quote(path: &Path) -> String {
        format!("'{}'", path.display().to_string().replace('\'', r"'\''"))
    }

    fn echo_bin() -> &'static str {
        if Path::new("/bin/echo").exists() {
            "/bin/echo"
        } else {
            "/usr/bin/echo"
        }
    }

    fn sleep_bin() -> &'static str {
        if Path::new("/bin/sleep").exists() {
            "/bin/sleep"
        } else {
            "/usr/bin/sleep"
        }
    }

    fn process_alive(pid: libc::pid_t) -> bool {
        let Ok(status) = std::fs::read_to_string(format!("/proc/{pid}/status")) else {
            return false;
        };
        !status.lines().any(|line| {
            line.starts_with("State:") && (line.contains('Z') || line.contains("zombie"))
        })
    }

    #[test]
    fn volume_listener_does_not_fork_the_engine() {
        let call = concat!(".", "pre_exec");
        assert!(
            !include_str!("system.rs").contains(call),
            "the pactl subscribe loop must not fork the live engine"
        );
        assert!(
            !include_str!("pdeath.rs").contains(call),
            "the helper must be posix_spawn'd"
        );
    }

    #[test]
    fn helper_image_is_an_executable_elf() {
        assert!(HELPER.starts_with(b"\x7fELF"));
        let (dir, _rm) = scratch();
        let path = materialize(&dir).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), HELPER);
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(materialize(&dir).unwrap(), path);
    }

    #[test]
    fn spawn_execs_the_command_as_a_child_of_the_caller() {
        let (dir, _rm) = scratch();
        let helper = materialize(&dir).unwrap();
        let mut child = spawn_when_free(&helper, &[sleep_bin(), "30"]);
        let pid = child.id();
        let mut exe = PathBuf::new();
        for _ in 0..50 {
            if let Ok(link) = std::fs::read_link(format!("/proc/{pid}/exe")) {
                exe = link;
                if exe.ends_with("sleep") {
                    break;
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let status = std::fs::read_to_string(format!("/proc/{pid}/status")).unwrap_or_default();
        let _ = child.kill();
        let _ = child.wait();
        assert!(
            exe.ends_with("sleep"),
            "child image is {exe:?}, not the spawned command"
        );
        let ppid = status
            .lines()
            .find(|line| line.starts_with("PPid:"))
            .unwrap_or("");
        let me = std::process::id().to_string();
        assert_eq!(
            ppid.split_whitespace().nth(1),
            Some(me.as_str()),
            "parent should be the spawner, got {ppid}"
        );
    }

    #[test]
    fn helper_runs_the_command_and_returns_its_output() {
        let (dir, _rm) = scratch();
        let helper = materialize(&dir).unwrap();
        let mut child = spawn_when_free(&helper, &[echo_bin(), "ok"]);
        let mut out = String::new();
        child
            .stdout
            .take()
            .unwrap()
            .read_to_string(&mut out)
            .unwrap();
        assert_eq!(out, "ok\n");
        assert!(child.wait().unwrap().success());
    }

    #[test]
    fn listener_dies_when_its_parent_exits() {
        let (dir, _rm) = scratch();
        let helper = materialize(&dir).unwrap();
        let pidfile = dir.join("pid");
        let errfile = dir.join("err");
        let script = format!(
            "{helper} {sleep} 30 &\n\
             pid=$!\n\
             echo \"$pid\" > {pidfile}\n\
             sleep 0.2\n\
             if ! kill -0 \"$pid\" 2>/dev/null; then echo early > {errfile}; exit 1; fi\n\
             exit 0\n",
            helper = shell_quote(&helper),
            sleep = shell_quote(Path::new(sleep_bin())),
            pidfile = shell_quote(&pidfile),
            errfile = shell_quote(&errfile),
        );
        let status = Command::new("bash")
            .arg("-c")
            .arg(&script)
            .status()
            .unwrap();
        assert!(
            status.success(),
            "child died while its parent was still alive ({})",
            std::fs::read_to_string(&errfile).unwrap_or_default()
        );
        let pid: libc::pid_t = std::fs::read_to_string(&pidfile)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let mut gone = false;
        for _ in 0..50 {
            if !process_alive(pid) {
                gone = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(40));
        }
        if process_alive(pid) {
            // SAFETY: a plain signal to a pid this test started.
            unsafe { libc::kill(pid, libc::SIGKILL) };
        }
        assert!(gone, "child survived the parent that spawned it");
    }
}
