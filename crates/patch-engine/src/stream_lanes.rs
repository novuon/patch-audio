//! Non-real-time allocation of native lanes to exposed playback streams.
//! Reservations survive engine handover, but only live identities retain them.
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, Default, serde::Deserialize, serde::Serialize)]
pub struct Lanes {
    pub assigned: BTreeMap<String, String>,
    #[serde(default)]
    pub heard: BTreeSet<String>,
    /// Streams whose controls the user moved: their strips always show.
    #[serde(default)]
    pub touched: BTreeSet<String>,
    /// A stream that took over another's lane: old key → its successor. A
    /// browser reopens its playback streams (an echo-cancelling capture
    /// starting, a device change), and whatever named the old stream (a
    /// Remote voice lane) follows the chain to the stream playing now.
    #[serde(default)]
    pub succession: BTreeMap<String, String>,
    /// Lanes their stream just left. A new stream
    /// from the same process within [`VACATED_HOLD`] takes the lane back.
    #[serde(skip)]
    vacated: Vec<Reservation>,
    #[serde(default)]
    labels: BTreeMap<String, String>,
}

#[derive(Clone, Debug)]
struct Reservation {
    key: String,
    channel: String,
    label: String,
    since: std::time::Instant,
    heard: bool,
    touched: bool,
}

/// How long a closed stream's lane waits for its process's next stream.
pub const VACATED_HOLD: std::time::Duration = std::time::Duration::from_secs(10);

/// The client process a stream key belongs to (`stream:serial:client:process`).
pub fn process(key: &str) -> &str {
    key.rsplit_once(':').map_or("", |(_, pid)| pid)
}

impl Lanes {
    pub fn reconcile(&mut self, live: impl Iterator<Item = (String, String)>) -> Vec<String> {
        self.reconcile_labeled_at(live, std::time::Instant::now())
    }

    #[cfg(test)]
    fn reconcile_unlabeled(&mut self, live: impl Iterator<Item = String>) -> Vec<String> {
        self.reconcile_at(live, std::time::Instant::now())
    }

    #[cfg(test)]
    fn reconcile_at(&mut self, live: impl Iterator<Item = String>, now: std::time::Instant) -> Vec<String> {
        self.reconcile_labeled_at(live.map(|key| (key, String::new())), now)
    }

    fn reconcile_labeled_at(
        &mut self,
        live: impl Iterator<Item = (String, String)>,
        now: std::time::Instant,
    ) -> Vec<String> {
        let live: BTreeMap<_, _> = live.collect();
        let mut released = Vec::new();
        let mut used = BTreeSet::new();
        let mut gone = Vec::new();
        self.assigned.retain(|key, channel| {
            let keep = live.contains_key(key)
                && patch_core::stream::CHANNELS.contains(&channel.as_str())
                && used.insert(channel.clone());
            if !keep {
                released.push(channel.clone());
                gone.push((key.clone(), channel.clone()));
            }
            keep
        });
        released.retain(|channel| {
            patch_core::stream::CHANNELS.contains(&channel.as_str()) && !used.contains(channel)
        });
        self.vacated.retain(|lane| now.duration_since(lane.since) < VACATED_HOLD);
        for (key, channel) in gone {
            if released.contains(&channel) {
                self.vacated.push(Reservation {
                    heard: self.heard.remove(&key),
                    touched: self.touched.remove(&key),
                    label: self.labels.remove(&key).unwrap_or_default(),
                    key,
                    channel,
                    since: now,
                });
            }
        }
        let reserved: BTreeSet<String> = self.vacated.iter().map(|lane| lane.channel.clone()).collect();
        for (key, label) in &live {
            if self.assigned.contains_key(key) {
                continue;
            }
            // The lane its own process just left (the same stream, reopened).
            let pid = process(key);
            let back = self.vacated.iter().position(|lane| {
                !pid.is_empty() && process(&lane.key) == pid && lane.label == *label && !used.contains(&lane.channel)
            });
            if let Some(i) = back {
                let lane = self.vacated.remove(i);
                used.insert(lane.channel.clone());
                self.assigned.insert(key.clone(), lane.channel);
                if lane.heard { self.heard.insert(key.clone()); }
                if lane.touched { self.touched.insert(key.clone()); }
                self.inherit(&lane.key, key);
                continue;
            }
            // A free lane, not one held for a reopening stream, unless
            // nothing else is left.
            let free = |c: &&&str| !used.contains(**c);
            if let Some(channel) = patch_core::stream::CHANNELS.iter().filter(free).find(|c| !reserved.contains(**c))
                .or_else(|| patch_core::stream::CHANNELS.iter().find(free))
            {
                used.insert(channel.to_string());
                self.vacated.retain(|lane| lane.channel != *channel);
                self.assigned.insert(key.clone(), channel.to_string());
            }
        }
        self.heard.retain(|key| self.assigned.contains_key(key));
        self.touched.retain(|key| self.assigned.contains_key(key));
        self.labels = live;
        self.prune_succession();
        released
    }

    pub fn inherited(&self, key: &str) -> bool {
        self.succession.values().any(|successor| successor == key)
    }

    /// Resolve a voice binding to the successor carrying its audio.
    pub fn current<'a>(&'a self, key: &'a str) -> Option<&'a str> {
        let current = self.successor(key)?;
        self.assigned.contains_key(current).then_some(current)
    }

    fn successor<'a>(&'a self, key: &'a str) -> Option<&'a str> {
        let mut current = key;
        for _ in 0..32 {
            match self.succession.get(current) {
                Some(next) => current = next,
                None => return Some(current),
            }
        }
        None
    }

    /// `new` takes `old`'s lane while both live (a handover: the sound
    /// moved to a new stream of the same process). The lanes swap, so the
    /// strip, its settings and anything bound to `old` follow the sound.
    pub fn swap(&mut self, old: &str, new: &str) -> Option<(String, String)> {
        let a = self.assigned.get(old)?.clone();
        let b = self.assigned.get(new)?.clone();
        self.assigned.insert(old.to_string(), b.clone());
        self.assigned.insert(new.to_string(), a.clone());
        let (old_heard, new_heard) = (self.heard.contains(old), self.heard.contains(new));
        let (old_touched, new_touched) = (self.touched.contains(old), self.touched.contains(new));
        for (set, o, n) in [(&mut self.heard, old_heard, new_heard), (&mut self.touched, old_touched, new_touched)] {
            set.remove(old);
            set.remove(new);
            if o { set.insert(new.to_string()); }
            if n { set.insert(old.to_string()); }
        }
        self.succession.remove(new);
        self.succession.insert(old.to_string(), new.to_string());
        for v in self.succession.values_mut() {
            if v == old { *v = new.to_string(); }
        }
        Some((a, b))
    }

    /// Undoes [`Self::swap`] (both streams turned out to play: two sources).
    pub fn unswap(&mut self, old: &str, new: &str) {
        if self.succession.get(old).map(String::as_str) == Some(new) {
            self.swap(new, old);
            self.succession.remove(old);
            self.succession.remove(new);
        }
    }

    fn inherit(&mut self, old: &str, new: &str) {
        self.succession.insert(old.to_string(), new.to_string());
        for v in self.succession.values_mut() {
            if v == old { *v = new.to_string(); }
        }
    }

    fn prune_succession(&mut self) {
        let alive: BTreeSet<String> = self.succession.keys()
            .filter(|k| self.successor(k).is_some_and(|current| {
                self.assigned.contains_key(current)
                    || self.vacated.iter().any(|lane| lane.key == current)
            }))
            .cloned()
            .collect();
        self.succession.retain(|k, _| alive.contains(k));
    }
}

/// A browser moved its sound to a new stream: `new` is heard for the first
/// time while `old`, of the same process and application, fell quiet just
/// now (heard within 3 s, silent for the last 0.4 s) and carries an identity
/// worth keeping (heard, adjusted, or a Remote voice). A tab starting while
/// another plays, or after a long silence, is a new source, not a handover.
pub fn handover(
    same_source: bool,
    new_first_heard: bool,
    new_touched: bool,
    old_identity: bool,
    old_heard_within_3s: bool,
    old_heard_within_0_4s: bool,
) -> bool {
    same_source && new_first_heard && !new_touched && old_identity && old_heard_within_3s && !old_heard_within_0_4s
}

/// An application's last playback-lane settings. A browser closes an idle
/// stream after a few seconds and opens a new one on play; the new stream
/// takes these, so pausing a tab does not reset its fader, EQ or sends.
/// Mute, solo and PFL are not carried: a new caller never arrives muted.
#[derive(Clone, Debug, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct Profile {
    pub gain: f32,
    pub pan: f32,
    pub trim: f32,
    /// Strip EQ (low, mid, high, filter).
    pub eq: [f32; 4],
    /// Per bus: (level, pre-fader).
    pub sends: [(f32, bool); crate::BUSES],
    /// The eight-band EQ, kept by the EQ store.
    #[serde(default)]
    pub eq8: Option<patch_core::eq::EqState>,
}

impl Profile {
    pub fn of(p: &crate::Params, c: usize) -> Profile {
        let eq = p.eq_of(c);
        Profile {
            gain: p.gain_of(c),
            pan: f32::from_bits(p.pan[c].load(std::sync::atomic::Ordering::Relaxed)),
            trim: p.trim_of(c),
            eq: [eq.low, eq.mid, eq.high, eq.filter],
            sends: std::array::from_fn(|b| (p.send_of(c, b), p.send_pre_of(c, b))),
            eq8: None,
        }
    }

    /// Seed a freshly reset lane. `stream_gain` is the new stream's own
    /// Pulse level, which the lane takes over before it returns to unity.
    pub fn apply(&self, p: &crate::Params, c: usize, stream_gain: f32) {
        p.set_gain(c, self.gain * stream_gain);
        p.set_pan(c, self.pan);
        p.set_trim(c, self.trim);
        let [low, mid, high, filter] = self.eq;
        p.set_eq(c, crate::eq::Settings { low, mid, high, filter });
        for (b, (level, pre)) in self.sends.iter().enumerate() {
            p.set_send(c, b, *level);
            p.set_send_pre(c, b, *pre);
        }
    }
}

pub fn profiles_path() -> std::path::PathBuf {
    crate::session::state_dir().join("stream-profiles.json")
}

pub fn load_profiles() -> BTreeMap<String, Profile> {
    std::fs::read(profiles_path()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

pub fn save_profiles(profiles: &BTreeMap<String, Profile>) {
    let path = profiles_path();
    if let Ok(bytes) = serde_json::to_vec_pretty(profiles) {
        let _ = std::fs::create_dir_all(crate::session::state_dir());
        let temp = path.with_extension("json.new");
        if std::fs::write(&temp, bytes).is_ok() {
            let _ = std::fs::rename(temp, path);
        }
    }
}

/// The lane that speaks for each application: of its streams that were
/// heard or adjusted, the one heard most recently. `lanes` holds
/// (application, stream key, signal age in cycles, touched).
pub fn representatives<'a>(lanes: impl Iterator<Item = (&'a str, &'a str, Option<u32>, bool)>) -> BTreeMap<String, String> {
    let mut best: BTreeMap<String, (u64, String)> = BTreeMap::new();
    for (app, key, age, touched) in lanes {
        if age.is_none() && !touched {
            continue;
        }
        let rank = age.map_or(u64::MAX, u64::from);
        if best.get(app).is_none_or(|(r, _)| rank < *r) {
            best.insert(app.to_string(), (rank, key.to_string()));
        }
    }
    best.into_iter().map(|(app, (_, key))| (app, key)).collect()
}

/// How long a quiet browser stream keeps its Mixer strip after its last
/// signal while another stream of the same browser is the one being heard.
pub const QUIET_HOLD_SECONDS: f32 = 60.0;

/// Generic browser streams the Mixer leaves out. Every stream keeps its
/// native lane and its Router entry; only the strip waits. A stream shows
/// once it carried a second of signal, unless a sibling in the same browser
/// process was heard more recently and this one has been quiet for
/// [`QUIET_HOLD_SECONDS`]. A stream the user adjusted always shows.
/// `streams` holds (key, helper candidate, signal age in cycles, quiet
/// for the hold, touched).
pub fn hidden_helpers(streams: &[(String, bool, Option<u32>, bool, bool)]) -> BTreeSet<String> {
    let process = |key: &str| key.rsplit_once(':').map_or("", |(_, pid)| pid).to_string();
    streams.iter().filter(|(key, candidate, age, quiet, touched)| {
        if !candidate || *touched {
            return false;
        }
        let Some(age) = age else { return true };
        *quiet && streams.iter().any(|(other, _, other_age, _, _)| {
            other != key && process(other) == process(key) && other_age.is_some_and(|a| a < *age)
        })
    }).map(|(key, ..)| key.clone()).collect()
}

/// Only indistinguishable visible sources need a number. Preserve source titles.
pub fn display_labels(labels: &BTreeMap<String, String>, hidden: &BTreeSet<String>) -> BTreeMap<String, String> {
    let mut groups: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for (key, label) in labels { if !hidden.contains(key) { groups.entry(label).or_default().push(key); } }
    let mut result = labels.clone();
    for (label, keys) in groups {
        for (i, key) in keys.iter().enumerate() {
            result.insert((*key).into(), if keys.len() > 1 { format!("{label} {}", i + 1) } else { label.into() });
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn shared_clients_get_independent_stable_lanes_and_release_only_the_departed_stream() {
        let a = patch_core::stream::key(100, Some(42), "900");
        let b = patch_core::stream::key(101, Some(42), "900");
        let c = patch_core::stream::key(102, Some(42), "900");
        let mut lanes = Lanes::default();
        lanes.reconcile_unlabeled([a.clone(), b.clone()].into_iter());
        let first = lanes.assigned[&a].clone();
        let second = lanes.assigned[&b].clone();
        assert_ne!(first, second);
        lanes.reconcile_unlabeled([b.clone(), a.clone(), c.clone()].into_iter());
        assert_eq!(lanes.assigned[&a], first);
        assert_eq!(lanes.assigned[&b], second);
        let mut restored: Lanes =
            serde_json::from_str(&serde_json::to_string(&lanes).unwrap()).unwrap();
        assert_eq!(restored.reconcile_unlabeled([b.clone(), c].into_iter()), [first]);
        assert_eq!(restored.assigned[&b], second);
    }
    #[test]
    fn a_reopened_stream_takes_back_its_lane_and_whatever_named_it_follows() {
        let t = std::time::Instant::now();
        let space = patch_core::stream::key(100, Some(7), "310935");
        let other = patch_core::stream::key(101, Some(7), "310935");
        let mut lanes = Lanes::default();
        lanes.reconcile_at([space.clone(), other.clone()].into_iter(), t);
        let lane = lanes.assigned[&space].clone();
        lanes.touched.insert(space.clone());
        // Chromium closes the Space's stream (one pass), opens a new one (next).
        lanes.reconcile_at(std::iter::once(other.clone()), t);
        let reopened = patch_core::stream::key(140, Some(7), "310935");
        let stranger = patch_core::stream::key(141, Some(9), "555");
        lanes.reconcile_at([other.clone(), stranger.clone(), reopened.clone()].into_iter(), t + std::time::Duration::from_secs(2));
        assert_eq!(lanes.assigned[&reopened], lane, "the same lane, not a new strip");
        assert_ne!(lanes.assigned[&stranger], lane, "another process never takes a held lane");
        assert!(lanes.touched.contains(&reopened));
        assert_eq!(lanes.current(&space), Some(reopened.as_str()), "a Remote voice bound to the old stream follows");
        // Held only briefly: later, the lane is anyone's.
        lanes.reconcile_at([other.clone(), stranger.clone()].into_iter(), t + std::time::Duration::from_secs(3));
        assert_eq!(lanes.current(&space), None);
        let late = patch_core::stream::key(150, Some(7), "310935");
        lanes.reconcile_at([other, stranger, late.clone()].into_iter(), t + std::time::Duration::from_secs(30));
        assert!(lanes.succession.is_empty());
    }

    #[test]
    fn a_handover_swaps_lanes_and_two_real_sources_swap_back() {
        let old = patch_core::stream::key(1, Some(7), "310935");
        let new = patch_core::stream::key(2, Some(7), "310935");
        let mut lanes = Lanes::default();
        lanes.reconcile_unlabeled([old.clone(), new.clone()].into_iter());
        let (a, b) = (lanes.assigned[&old].clone(), lanes.assigned[&new].clone());
        lanes.touched.insert(old.clone());
        assert!(handover(true, true, false, true, true, false));
        lanes.swap(&old, &new);
        assert_eq!((lanes.assigned[&new].clone(), lanes.assigned[&old].clone()), (a.clone(), b.clone()));
        assert!(lanes.touched.contains(&new) && !lanes.touched.contains(&old));
        assert_eq!(lanes.current(&old), Some(new.as_str()), "voice bindings follow the sound");
        assert_eq!(lanes.succession.get(&old), Some(&new));
        lanes.unswap(&old, &new);
        assert_eq!((lanes.assigned[&old].clone(), lanes.assigned[&new].clone()), (a, b));
        assert!(lanes.touched.contains(&old) && lanes.succession.is_empty());
        assert_eq!(lanes.current(&old), Some(old.as_str()));
    }

    #[test]
    fn a_new_tab_is_no_handover() {
        // The other stream still plays, or was quiet long before, or the
        // new one is from another process: no swap.
        assert!(!handover(true, true, false, true, true, true));
        assert!(!handover(true, true, false, true, false, false));
        assert!(!handover(false, true, false, true, true, false));
        assert!(!handover(true, false, false, true, true, false), "only when first heard");
        assert!(!handover(true, true, false, false, true, false), "nothing to keep");
    }

    #[test]
    fn capacity_never_merges_streams_and_a_reused_id_from_another_process_is_new() {
        let mut lanes = Lanes::default();
        lanes.reconcile_unlabeled((0..40).map(|i| patch_core::stream::key(i, Some(1), "2")));
        assert_eq!(lanes.assigned.len(), 32);
        assert_eq!(lanes.assigned.values().collect::<BTreeSet<_>>().len(), 32);
        let new = patch_core::stream::key(0, Some(1), "3");
        assert_eq!(lanes.reconcile_unlabeled(std::iter::once(new.clone())).len(), 32);
        assert_eq!(lanes.assigned.len(), 1);
        assert!(lanes.assigned.contains_key(&new));
    }
}

#[cfg(test)]
mod presentation_tests {
    use super::*;
    #[test]
    fn a_silent_sibling_hides_but_a_quiet_caller_alone_or_adjusted_stays() {
        let a = patch_core::stream::key(1, Some(10), "77");
        let b = patch_core::stream::key(2, Some(11), "77");
        let other = patch_core::stream::key(3, Some(12), "88");
        // Never heard: hidden. Heard recently: shown.
        let h = hidden_helpers(&[(a.clone(), true, None, true, false), (b.clone(), true, Some(5), false, false)]);
        assert_eq!(h, [a.clone()].into());
        // Heard once at join, quiet past the hold, the sibling carries the Space.
        let h = hidden_helpers(&[(a.clone(), true, Some(90_000), true, false), (b.clone(), true, Some(5), false, false)]);
        assert_eq!(h, [a.clone()].into());
        // The Space paused: it was still heard last, so its strip stays.
        let h = hidden_helpers(&[(a.clone(), true, Some(90_000), true, false), (b.clone(), true, Some(20_000), true, false)]);
        assert_eq!(h, [a.clone()].into());
        // Another process's louder stream does not hide a quiet caller.
        let h = hidden_helpers(&[(a.clone(), true, Some(90_000), true, false), (other.clone(), true, Some(5), false, false)]);
        assert!(h.is_empty());
        // Adjusted by the user: always shown, even never heard.
        let h = hidden_helpers(&[(a.clone(), true, None, true, true), (b, true, Some(5), false, false)]);
        assert!(h.is_empty());
        // A titled stream is never a helper.
        assert!(hidden_helpers(&[(other, false, None, true, false)]).is_empty());
    }
    #[test]
    fn the_most_recently_heard_or_adjusted_stream_speaks_for_its_app() {
        let reps = representatives([
            ("Chromium", "helper", None, false),
            ("Chromium", "space", Some(3), false),
            ("Chromium", "old", Some(900), true),
            ("Teams", "call", None, true),
        ].into_iter());
        assert_eq!(reps["Chromium"], "space");
        assert_eq!(reps["Teams"], "call");
    }
    #[test]
    fn a_reopened_stream_takes_the_apps_fader_eq_and_sends_but_not_mute() {
        let p = crate::Params::new(&["stream-1", "stream-2"]);
        p.set_gain(0, 0.25);
        p.set_pan(0, -0.5);
        p.set_eq(0, crate::eq::Settings { low: 0.5, mid: 1.0, high: 2.0, filter: 0.1 });
        p.set_send(0, 1, 0.7);
        p.set_mute(0, true);
        let profile = Profile::of(&p, 0);
        let back: Profile = serde_json::from_str(&serde_json::to_string(&profile).unwrap()).unwrap();
        assert_eq!(back, profile);
        back.apply(&p, 1, 1.0);
        assert_eq!(p.gain_of(1), 0.25);
        assert_eq!(p.eq_of(1).high, 2.0);
        assert_eq!(p.send_of(1, 1), 0.7);
        assert!(!p.is_muted(1));
    }
    #[test]
    fn silent_helpers_do_not_number_the_real_source_and_titles_are_preserved() {
        let labels = [("a".into(), "Chromium".into()), ("b".into(), "Chromium".into()),
            ("c".into(), "Chromium · Space".into())].into();
        let visible = display_labels(&labels, &["a".into()].into());
        assert_eq!(visible["b"], "Chromium");
        assert_eq!(visible["c"], "Chromium · Space");
        assert_eq!(display_labels(&labels, &BTreeSet::new())["b"], "Chromium 2");
    }
    #[test]
    fn heard_identity_survives_handover_but_not_lane_reuse() {
        let mut lanes = Lanes::default();
        lanes.reconcile_unlabeled(["old".into()].into_iter());
        lanes.heard.insert("old".into());
        let mut restored: Lanes = serde_json::from_str(&serde_json::to_string(&lanes).unwrap()).unwrap();
        restored.reconcile_unlabeled(["old".into()].into_iter());
        assert!(restored.heard.contains("old"));
        restored.reconcile_unlabeled(["new".into()].into_iter());
        assert!(restored.heard.is_empty());
    }
}
