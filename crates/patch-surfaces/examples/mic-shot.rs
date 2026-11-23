//! Headless Mic layout QA, with clearly simulated engine values and no audio.
use patch_surfaces::ui::{AudioState, InspRow, InspSection, PatchWindow};
use slint::ComponentHandle;
fn main() -> Result<(), Box<dyn std::error::Error>> {
    patch_host::shot::install()?;
    let window = PatchWindow::new()?;
    window.set_surface("mic".into());
    let state = window.global::<AudioState>();
    state.set_live(false);
    state.set_selected("engine.mic-1".into());
    state.set_sel_name("Mic 1".into());
    state.set_sel_kind("Microphone".into());
    state.set_sel_detail("Simulated universal input · channel 1".into());
    state.set_sel_hardware(true);
    state.set_sel_level(0.72);
    state.set_mic_history(
        std::rc::Rc::new(slint::VecModel::from(
            (0..128)
                .map(|i| 0.25 + 0.5 * (i as f32 * 0.15).sin().abs())
                .collect::<Vec<_>>(),
        ))
        .into(),
    );
    state.set_mic_history_span("1,355 ms · engine cycles →".into());
    let row = |kind: &str, label: &str, value: &str, amount: f32, active: bool| InspRow {
        kind: kind.into(),
        label: label.into(),
        value: value.into(),
        amount,
        active,
    };
    let section = |title: &str, rows: Vec<InspRow>| InspSection {
        title: title.into(),
        rows: std::rc::Rc::new(slint::VecModel::from(rows)).into(),
    };
    state.set_sections(
        std::rc::Rc::new(slint::VecModel::from(vec![
            section(
                "Level",
                vec![
                    row("live-meter", "Output", "live", 0.0, true),
                    row("gain", "Software gain", "0 dB", 0.8, true),
                ],
            ),
            section(
                "Processing state",
                vec![
                    row("kv", "Raw input peak", "−12.0 dBFS", 0.0, false),
                    row("kv", "Compressor reduction", "3.2 dB", 0.0, false),
                ],
            ),
            section(
                "Voice",
                vec![
                    row("voice", "High-pass", "80 Hz", 0.25, true),
                    row("voice", "Noise gate", "−50 dBFS", 0.5, true),
                    row("voice", "Expander", "off", 0.5, false),
                    row("voice", "Compressor", "50 %", 0.5, true),
                ],
            ),
        ]))
        .into(),
    );
    let dir = std::path::PathBuf::from(
        std::env::args()
            .nth(1)
            .unwrap_or_else(|| "/tmp/patch-mic-shots".into()),
    );
    std::fs::create_dir_all(&dir)?;
    let shooter = patch_host::shot::Shooter { scale: 1.0 };
    for (width, height) in [(560, 460), (900, 600), (1600, 720)] {
        shooter
            .render_component(&window, width, height)
            .save(dir.join(format!("mic-{width}x{height}.png")))?;
    }
    Ok(())
}
