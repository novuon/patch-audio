//! Library reads run off the UI thread; all windows share the delivered data.

use crate::{app::Brief, dj::LibraryItem};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::time::{Duration, Instant};

type Ready = HashMap<String, (patch_library::Readiness, Option<String>)>;

pub struct Rows {
    pub items: Vec<LibraryItem>,
    /// Lowercased title, artist line, and path, for the search box.
    pub keys: Vec<(String, String, String)>,
}

#[derive(Default)]
pub struct Snapshot {
    pub status: Option<patch_engine::librarian::Status>,
    pub items: Option<Rows>,
    pub ready: Option<Ready>,
    pub briefs: Option<HashMap<String, Brief>>,
    pub collections: Option<Vec<(i64, String, usize)>>,
    pub members: Option<HashMap<i64, HashSet<String>>>,
    /// The set preps, and each one's tracks in order.
    /// Set preps, the listed order of every collection shown in order (set
    /// preps and those played in order), and how each one plays (true: in
    /// order) where chosen.
    pub sets: Option<(HashSet<i64>, HashMap<i64, Vec<String>>, HashMap<i64, bool>)>,
}

struct Worker {
    wake: mpsc::SyncSender<()>,
    force: Arc<AtomicBool>,
    details: Arc<AtomicBool>,
    latest: Arc<Mutex<Option<Snapshot>>>,
}

#[derive(Default)]
pub struct Loader(RefCell<Option<Worker>>);

impl Loader {
    pub fn request(&self, force: bool, details: bool) {
        let mut slot = self.0.borrow_mut();
        if slot.is_none() {
            let (wake, rx) = mpsc::sync_channel(1);
            let latest = Arc::new(Mutex::new(None::<Snapshot>));
            let shared = latest.clone();
            let forced = Arc::new(AtomicBool::new(true));
            let reload = forced.clone();
            let interested = Arc::new(AtomicBool::new(details));
            let interest = interested.clone();
            let started = std::thread::Builder::new()
                .name("patch-library-read".into())
                .spawn(move || {
                    let mut signature = None;
                    let mut history_minute = 0;
                    let mut checked = None::<Instant>;
                    let mut counts = None;
                    let mut paths = Vec::<String>::new();
                    let mut had_details = false;
                    // Each track's brief, and the analysis file's stamp it
                    // was made from: only a changed file is read again. (All
                    // 1 700 were read and parsed whenever the librarian's
                    // counts changed, every few seconds while it worked:
                    // ~15 000 reads in 7 minutes, the heap growing.)
                    let mut known: HashMap<
                        String,
                        (Option<patch_analysis::CacheStamp>, Option<Brief>),
                    > = HashMap::new();
                    // Where each track's analysis file lives, by the audio
                    // file's stamp: finding it canonicalises the path and
                    // reads the file's content key, and checking it is the
                    // track's record parses it. Both only when something
                    // changed, not for every track on every pass.
                    let mut located: HashMap<
                        String,
                        (Option<patch_analysis::CacheStamp>, Option<std::path::PathBuf>),
                    > = HashMap::new();
                    while rx.recv().is_ok() {
                        let force = reload.swap(false, Ordering::Relaxed);
                        let details = interest.load(Ordering::Relaxed);
                        let status = patch_engine::librarian::read_status();
                        let now_counts = status.as_ref().map(|s| {
                            s.sources
                                .iter()
                                .map(|x| (x.path.clone(), x.counts, x.connected))
                                .collect::<Vec<_>>()
                        });
                        let counts_changed = now_counts != counts;
                        let mut update = Snapshot {
                            status,
                            ..Default::default()
                        };
                        if let Ok(db) = patch_library::open_quick() {
                            let due = force
                                || checked.is_none_or(|at| at.elapsed() >= Duration::from_secs(3));
                            let mut changed = false;
                            if due {
                                let sig = db.signature();
                                changed = signature != Some(sig);
                                signature = Some(sig);
                                checked = Some(Instant::now());
                            }
                            let minute = patch_engine::session::now_s() / 60;
                            if force || changed || (details && minute != history_minute) {
                                history_minute = minute;
                                if let Ok(items) = crate::dj::library_result() {
                                    paths = items
                                        .iter()
                                        .filter(|i| !i.url.contains("://"))
                                        .map(|i| i.url.clone())
                                        .collect();
                                    let keys = items
                                        .iter()
                                        .map(|i| {
                                            (
                                                i.title.to_lowercase(),
                                                // The years and genre match too: "1987", "deep house".
                                                format!(
                                                    "{} {} {} {}",
                                                    i.detail.to_lowercase(),
                                                    i.year,
                                                    i.year_release,
                                                    i.genre.to_lowercase()
                                                ),
                                                i.url.to_lowercase(),
                                            )
                                        })
                                        .collect();
                                    update.items = Some(Rows { items, keys });
                                    update.collections = Some(db.collections());
                                    update.members = db.collection_memberships().ok();
                                    let sets = db.set_preps();
                                    let plays = db.play_orders();
                                    let order = sets
                                        .iter()
                                        .chain(plays.iter().filter(|(_, o)| **o).map(|(id, _)| id))
                                        .map(|id| (*id, db.collection_paths(*id)))
                                        .collect();
                                    update.sets = Some((sets, order, plays));
                                } else {
                                    reload.store(true, Ordering::Relaxed);
                                }
                            }
                            if details && (force || changed || counts_changed || !had_details) {
                                update.ready = Some(db.readiness(patch_analysis::GENERATION));
                                let mut briefs = HashMap::with_capacity(paths.len());
                                for p in &paths {
                                    let audio = std::path::Path::new(p);
                                    let audio_stamp = patch_analysis::file_stamp(audio);
                                    let place = match located.get(p) {
                                        Some((a, l)) if audio_stamp.is_some() && *a == audio_stamp => {
                                            l.clone()
                                        }
                                        _ => {
                                            let l = patch_analysis::analysis_cache_location(audio);
                                            located.insert(p.clone(), (audio_stamp, l.clone()));
                                            l
                                        }
                                    };
                                    let stamp =
                                        place.as_deref().and_then(patch_analysis::file_stamp);
                                    let brief = match known.get(p) {
                                        Some((s, b)) if *s == stamp => b.clone(),
                                        _ => {
                                            // Changed: is the record there
                                            // this file's (moved, copied)?
                                            let file = patch_analysis::analysis_cache_path(audio);
                                            // The cache file is the normal
                                            // source; when it is gone, read
                                            // the analysis the library stored
                                            // instead of leaving the row blank.
                                            // Memoised on the same stamp, so a
                                            // missing cache costs one read, not
                                            // one per poll.
                                            let b = file
                                                .as_deref()
                                                .and_then(patch_analysis::read_file)
                                                .or_else(|| {
                                                    db.analysis_doc(p).and_then(|doc| {
                                                        serde_json::from_str(&doc).ok()
                                                    })
                                                })
                                                .map(|a| Brief::of(&a));
                                            known.insert(p.clone(), (stamp, b.clone()));
                                            b
                                        }
                                    };
                                    if let Some(b) = brief {
                                        briefs.insert(p.clone(), b);
                                    }
                                }
                                let present: HashSet<&String> = paths.iter().collect();
                                known.retain(|p, _| present.contains(p));
                                located.retain(|p, _| present.contains(p));
                                update.briefs = Some(briefs);
                            }
                            counts = now_counts;
                            had_details = details;
                        } else if force {
                            // Retry a forced load once the library becomes readable.
                            reload.store(true, Ordering::Relaxed);
                        }
                        // Retain undelivered row updates when a status-only update
                        // arrives before the UI consumes the previous snapshot.
                        let mut pending = shared.lock().unwrap();
                        if let Some(old) = pending.as_mut() {
                            old.status = update.status;
                            if update.items.is_some() {
                                old.items = update.items;
                            }
                            if update.ready.is_some() {
                                old.ready = update.ready;
                            }
                            if update.briefs.is_some() {
                                old.briefs = update.briefs;
                            }
                            if update.collections.is_some() {
                                old.collections = update.collections;
                            }
                            if update.members.is_some() {
                                old.members = update.members;
                            }
                            if update.sets.is_some() {
                                old.sets = update.sets;
                            }
                        } else {
                            *pending = Some(update);
                        }
                    }
                });
            if started.is_err() {
                return;
            }
            *slot = Some(Worker {
                wake,
                force: forced,
                details: interested,
                latest,
            });
        }
        let worker = slot.as_ref().unwrap();
        if force {
            worker.force.store(true, Ordering::Relaxed);
        }
        worker.details.store(details, Ordering::Relaxed);
        let _ = worker.wake.try_send(());
    }

    pub fn take(&self) -> Option<Snapshot> {
        self.0.borrow().as_ref()?.latest.try_lock().ok()?.take()
    }
}
