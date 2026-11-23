//! Offline A/B rendering through exactly the native engine processors and
//! Recorder's WAV writer. Never runs on the audio callback.
use std::path::Path;

pub fn render(
    samples: &[f32],
    profile: &patch_core::mic::Profile,
    output: &Path,
) -> Result<(), String> {
    profile.validate()?;
    if !(48000..=48000 * 60).contains(&samples.len()) || samples.iter().any(|s| !s.is_finite()) {
        return Err("use a finite 1–60 second mono take decoded at 48 kHz".into());
    }
    let mut l = samples.to_vec();
    let mut r = l.clone();
    let mut voice = patch_engine::voice::Voice::default();
    let published = patch_engine::master_eq::Published::default();
    published.set(&profile.eq);
    let mut eq = patch_engine::master_eq::MasterEq::default();
    let mut limiter =
        patch_engine::limiter::Limiter::new_zero_latency(patch_engine::MASTER_CEILING_DBFS, 48000);
    for (left, right) in l.chunks_mut(256).zip(r.chunks_mut(256)) {
        voice.process(left, right, &profile.voice, 48000);
        if eq.prepare(&published, 48000) {
            eq.run_side(0, left);
            eq.run_side(1, right);
        }
        limiter.process_mode(left, right, false);
    }
    let mut wav = crate::wav::WavWriter::create(
        output,
        crate::wav::SampleFormat::F32,
        1,
        48000,
        "Patch Mic processed comparison",
    )
    .map_err(|e| e.to_string())?;
    for chunk in l.chunks(4096) {
        let bytes: Vec<u8> = chunk.iter().flat_map(|s| s.to_le_bytes()).collect();
        wav.write(&bytes).map_err(|e| e.to_string())?;
    }
    wav.finish(None).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn render_protects_existing_files_and_rejects_bad_samples() {
        let dir = std::env::temp_dir().join(format!("patch-mic-render-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("take.wav");
        let profile = patch_core::mic::Profile::new(
            "Raw",
            patch_core::mic::Settings::OFF,
            Default::default(),
        );
        let input = vec![0.2; 48000];
        render(&input, &profile, &path).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        assert!(render(&input, &profile, &path).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        assert!(render(&[f32::NAN; 48000], &profile, &dir.join("bad.wav")).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
