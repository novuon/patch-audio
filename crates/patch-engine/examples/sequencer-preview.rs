//! Offline preview of the three factory grooves (four bars each).
//! cargo run -p patch-engine --example sequencer-preview -- /tmp/grooves.wav
use patch_engine::sequencer::{Pattern, Sequencer, Shared};
use std::io::Write;
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args()
        .nth(1)
        .unwrap_or("sequencer-preview.wav".into());
    let mut samples = Vec::<f32>::new();
    for preset in 0..3 {
        let pattern = Pattern::preset(preset);
        let frames = (48000.0 * 60.0 / pattern.bpm as f64 * 16.0) as usize;
        let p = Shared::default();
        p.request(&serde_json::json!({"pattern":pattern, "playing":true}))?;
        let mut seq = Sequencer::default();
        let mut l = vec![0.0; frames];
        let mut r = l.clone();
        let (mut pl, mut pr) = (vec![0.0; frames], vec![0.0; frames]);
        seq.render(&p, 48000, [&mut l, &mut r], [&mut pl, &mut pr]);
        samples.extend(l.into_iter().zip(r).flat_map(|(l, r)| [l, r]));
        p.request(&serde_json::json!({"stop":true}))?;
        let mut l = vec![0.0; 12000];
        let mut r = l.clone();
        let (mut pl, mut pr) = (vec![0.0; 12000], vec![0.0; 12000]);
        seq.render(&p, 48000, [&mut l, &mut r], [&mut pl, &mut pr]);
        samples.extend(l.into_iter().zip(r).flat_map(|(l, r)| [l, r]));
    }
    let mut file = std::io::BufWriter::new(std::fs::File::create(path)?);
    let bytes = samples.len() as u32 * 2;
    file.write_all(b"RIFF")?;
    file.write_all(&(36 + bytes).to_le_bytes())?;
    file.write_all(b"WAVEfmt ")?;
    file.write_all(&16u32.to_le_bytes())?;
    file.write_all(&1u16.to_le_bytes())?;
    file.write_all(&2u16.to_le_bytes())?;
    file.write_all(&48000u32.to_le_bytes())?;
    file.write_all(&192000u32.to_le_bytes())?;
    file.write_all(&4u16.to_le_bytes())?;
    file.write_all(&16u16.to_le_bytes())?;
    file.write_all(b"data")?;
    file.write_all(&bytes.to_le_bytes())?;
    for v in samples {
        file.write_all(&((v.clamp(-1.0, 1.0) * 32767.0) as i16).to_le_bytes())?;
    }
    Ok(())
}
