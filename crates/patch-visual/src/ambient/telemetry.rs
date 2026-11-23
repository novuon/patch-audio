//! One bounded cache for system scenes. The renderer never reads /proc or
//! /sys: a background worker publishes complete snapshots twice a second.
use std::{collections::HashMap,sync::{Arc,OnceLock,RwLock}};
#[derive(Default)]
struct Snapshot { files:HashMap<&'static str,String>, disks:Vec<String> }
#[cfg(not(test))]
fn shared() -> &'static Arc<RwLock<Snapshot>> {
    static DATA:OnceLock<Arc<RwLock<Snapshot>>>=OnceLock::new();
    DATA.get_or_init(|| {
        let data=Arc::new(RwLock::new(Snapshot::default()));
        let out=data.clone();
        let _=std::thread::Builder::new().name("patch-visual-system".into()).spawn(move || loop {
            let files=["/proc/stat","/proc/meminfo","/proc/loadavg","/proc/net/dev","/proc/diskstats"]
                .into_iter().filter_map(|p| std::fs::read_to_string(p).ok().map(|s|(p,s))).collect();
            let disks=std::fs::read_dir("/sys/block").map(|d| d.flatten().filter_map(|e| e.file_name().into_string().ok()).filter(|n| !["loop","ram","zram","dm-","md","sr"].iter().any(|p|n.starts_with(p))).collect()).unwrap_or_default();
            *out.write().unwrap_or_else(|e|e.into_inner())=Snapshot {files,disks};
            std::thread::sleep(std::time::Duration::from_millis(500));
        });
        data
    })
}
pub(super) fn read(path:&str)->Result<String,()> {
    shared().try_read().ok().and_then(|s|s.files.get(path).cloned()).ok_or(())
}
pub(super) fn disks()->Vec<String> { shared().try_read().map(|s|s.disks.clone()).unwrap_or_default() }

#[cfg(test)]
fn shared()->&'static Arc<RwLock<Snapshot>> {
    static DATA:OnceLock<Arc<RwLock<Snapshot>>>=OnceLock::new();
    DATA.get_or_init(|| Arc::new(RwLock::new(Snapshot {
        files:HashMap::from([
            ("/proc/stat","cpu 100 0 100 800 0 0 0 0\ncpu0 50 0 50 400 0 0 0 0\ncpu1 50 0 50 400 0 0 0 0\n".into()),
            ("/proc/meminfo","MemTotal: 16000000 kB\nMemAvailable: 8000000 kB\nSwapTotal: 4000000 kB\nSwapFree: 4000000 kB\n".into()),
            ("/proc/loadavg","0.5 0.5 0.5 1/100 1".into()),
            ("/proc/net/dev","header\nheader\neth0: 10000 1 0 0 0 0 0 0 20000 1 0 0 0 0 0 0".into()),
            ("/proc/diskstats","259 0 nvme0n1 1 0 10 0 1 0 20 0".into())
        ]),disks:vec!["nvme0n1".into()]
    })))
}
