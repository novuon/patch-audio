//! Persist actual deck playback off the audio and planning threads.
use super::*;

#[derive(Default)]
struct PlayedTrack {
    path: String,
    load: u32,
    heard: u32,
    recorded: bool,
}
impl PlayedTrack {
    fn observe(&mut self, path: &str, load: u32, audible: bool) -> bool {
        if self.path != path || self.load != load {
            *self = Self {
                path: path.into(),
                load,
                ..Self::default()
            };
        }
        if audible && !self.recorded && !path.is_empty() {
            self.heard += 1;
            return self.heard >= 10;
        }
        false
    }
}

// Use the same channel mute/solo/pan and Booth target as the audio mix.
// Output-device volume is deliberately excluded from play history.
fn main_mix_audible(params: &Params, ch: usize) -> bool {
    let booth = params.booth_gain();
    params
        .side_gains(ch)
        .into_iter()
        .any(|gain| gain * booth > 0.001)
}

pub(super) fn record_plays(s: Arc<Shared>) {
    let mut tracks: [PlayedTrack; 4] = std::array::from_fn(|_| PlayedTrack::default());
    loop {
        std::thread::sleep(Duration::from_secs(1));
        if s.retired.load(Ordering::Acquire) {
            continue;
        }
        for (d, track) in tracks.iter_mut().enumerate().take(s.params.decks.len()) {
            let (_, playing, path, ..) = deck_now(&s.params, d);
            let name = format!("deck-{}", (b'a' + d as u8) as char);
            let audible = playing
                && s.params
                    .channel(&name)
                    .is_some_and(|ch| main_mix_audible(&s.params, ch));
            let load = s.params.decks[d].loads.load(Ordering::Acquire);
            if track.observe(&path, load, audible) {
                let result = patch_library::open_default().and_then(|db| {
                    let id = db
                        .track_by_path(&path)
                        .ok_or("Playing track is outside the library")?;
                    db.log(
                        "play",
                        Some(&name),
                        Some(id),
                        &json!({"source":"engine","heard_seconds":track.heard}),
                    )
                });
                if result.is_ok() {
                    track.recorded = true;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn counts_heard_playback_once_per_loaded_track() {
        let mut t = PlayedTrack::default();
        for _ in 0..20 {
            assert!(!t.observe("a", 1, false));
        }
        for _ in 0..9 {
            assert!(!t.observe("a", 1, true));
        }
        assert!(t.observe("a", 1, true));
        t.recorded = true;
        assert!(!t.observe("a", 1, false));
        assert!(!t.observe("a", 1, true));
        assert!(!t.observe("b", 2, true));
        assert_eq!(t.heard, 1);
    }
    #[test]
    fn loading_the_same_file_again_is_a_new_play() {
        let mut t = PlayedTrack::default();
        for _ in 0..10 {
            t.observe("a", 1, true);
        }
        t.recorded = true;
        assert!(!t.observe("a", 2, true));
        assert_eq!(t.heard, 1);
        assert!(!t.recorded);
        for _ in 0..8 {
            assert!(!t.observe("a", 2, true));
        }
        assert!(t.observe("a", 2, true));
    }
    #[test]
    fn muted_solo_excluded_and_crossfader_silent_decks_do_not_count() {
        let p = Params::new(&["deck-a", "pattern", "mic-1"]);
        assert!(main_mix_audible(&p, 0));
        p.set_mute(0, true);
        assert!(!main_mix_audible(&p, 0));
        p.set_mute(0, false);
        p.set_solo(1, true);
        assert!(!main_mix_audible(&p, 0));
        p.set_dj_solo(true);
        assert!(main_mix_audible(&p, 0));
        p.set_solo(1, false);
        p.set_dj_solo(false);
        p.set_solo(2, true);
        assert!(
            main_mix_audible(&p, 0),
            "live mic solo preserves the programme"
        );
        p.set_solo(2, false);
        p.set_dj_group(1.0, true);
        assert!(!main_mix_audible(&p, 0));
        p.set_dj_group(0.0, false);
        assert!(!main_mix_audible(&p, 0));
        p.set_dj_group(1.0, false);
        let mixer = crate::session::Mixer {
            xfader: 1.0,
            ..Default::default()
        };
        p.set_gain(0, db_to_linear(mixer.gains_db()[0]));
        assert!(!main_mix_audible(&p, 0));
    }
}
