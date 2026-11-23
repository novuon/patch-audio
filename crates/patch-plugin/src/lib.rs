//! Patch's native VST3 layer. Plugins are loaded and controlled by Patch:
//! the engine runs the rack's active plugins as a chain on its master
//! (`chain`), and each open plugin window holds a mirror instance for its
//! controls, readouts and the plugin's own editor (`Mirror`).

pub mod chain;
pub mod config;
pub mod editor;
pub mod install;
pub mod scan;
pub mod shm;

pub use config::{
    CHANNEL_POINTS, MAX_SLOTS, POINTS, Place, RackConfig, SavedParameter, Slot, View, ZOOMS, channel_label, config_path, load_config, load_or_default,
    save_config,
};
pub use scan::BundleInfo;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use vst3_host::{RealtimePluginRunner, RtControl, Vst3Host, audio::AudioBuffers};

/// The engine's graph rate: plugins are prepared for it.
pub const RATE: u32 = 48_000;

/// The block a plugin is prepared for; longer blocks reach it in pieces.
/// Some plugins do the work of their prepared maximum on every call: Box
/// Of Bass took 8.4 ms for a 256-frame quantum prepared for 8192 (the
/// worker missed every block and the master stayed dry), 0.37 ms at 256.
pub const PREPARED_FRAMES: usize = 256;

/// VST3 `ParameterInfo` flags Patch reads.
const HIDDEN: u32 = 1 << 4;
const LIST: u32 = 1 << 3;
const PROGRAM_CHANGE: u32 = 1 << 15;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Parameter {
    pub id: u32,
    pub name: String,
    pub value: f64,
    pub default: f64,
    pub unit: String,
    /// Gaps between values: 0 continuous, 1 a switch, n a list of n + 1.
    pub steps: i32,
    pub read_only: bool,
    pub bypass: bool,
    pub list: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct PluginInfo {
    pub name: String,
    pub vendor: String,
    pub version: String,
    pub category: String,
    pub path: PathBuf,
    pub parameters: Vec<Parameter>,
}

/// One loaded plugin ready to render stereo blocks. The realtime runner
/// owns the plugin; `control` queues parameter changes to it lock-free.
pub struct Processor {
    // Field order is drop order: the runner hands the plugin back to the
    // control's teardown queue, which the control then services.
    runner: RealtimePluginRunner,
    control: Option<RtControl>,
    buffers: AudioBuffers,
    info: PluginInfo,
}

impl Drop for Processor {
    fn drop(&mut self) {
        let _ = self.runner.stop();
    }
}

fn host(rate: u32, max_frames: usize) -> Result<Vst3Host> {
    Vst3Host::builder()
        .sample_rate(f64::from(rate))
        .block_size(max_frames)
        .input_channels(2)
        .output_channels(2)
        .build()
        .context("create Patch VST3 host")
}

fn check(saved: &SavedParameter) -> Result<()> {
    anyhow::ensure!(
        saved.value.is_finite() && (0.0..=1.0).contains(&saved.value),
        "VST3 parameter {} is outside 0..=1",
        saved.id
    );
    Ok(())
}

impl Processor {
    /// Load a plugin before audio starts. The host reserves `max_frames` in
    /// advance so block processing never resizes its sample buffers.
    pub fn load(path: &Path, rate: u32, max_frames: usize) -> Result<Self> {
        Self::load_with_parameters(path, rate, max_frames, &[])
    }

    pub fn load_with_parameters(
        path: &Path,
        rate: u32,
        max_frames: usize,
        saved: &[SavedParameter],
    ) -> Result<Self> {
        anyhow::ensure!(max_frames > 0, "plugin block size must be nonzero");
        let mut plugin = host(rate, max_frames.min(PREPARED_FRAMES))?
            .load_plugin(path)
            .with_context(|| format!("load VST3 plugin at {}", path.display()))?;
        for parameter in saved {
            check(parameter)?;
            plugin
                .set_parameter(parameter.id, parameter.value)
                .with_context(|| format!("set VST3 parameter {}", parameter.id))?;
        }
        let info = inspect_loaded(&plugin, path)?;
        let (mut runner, control) = RealtimePluginRunner::new(plugin, 1024);
        runner.start().context("start VST3 processing")?;
        Ok(Self {
            runner,
            control: Some(control),
            buffers: AudioBuffers::new(2, 2, max_frames, f64::from(rate)),
            info,
        })
    }

    pub fn info(&self) -> &PluginInfo {
        &self.info
    }

    /// The control handle, for a processor that moves to another thread.
    /// It must be dropped (or serviced) on the thread that loaded the
    /// plugin, after the processor itself: that is where VST3 tears down.
    pub fn take_control(&mut self) -> Option<RtControl> {
        self.control.take()
    }

    /// Its editor, served on this (the loading) thread while another
    /// processes it (see [`vst3_host::EditorAccess`]).
    ///
    /// # Safety
    /// The handle must be dropped, its editor closed, before this
    /// processor is (wherever it then runs) and before its control's
    /// teardown runs.
    pub unsafe fn editor(&mut self) -> Option<vst3_host::EditorAccess> {
        // SAFETY: passed on to the caller.
        unsafe { self.runner.editor() }
    }

    /// Queue a normalized parameter update for the next block.
    pub fn set_parameter(&mut self, id: u32, value: f64) -> bool {
        if !value.is_finite() || !(0.0..=1.0).contains(&value) {
            return false;
        }
        self.control.as_mut().is_some_and(|c| c.set_parameter(id, value))
    }

    /// Process a stereo block in place. The fixed buffers are sized at load
    /// time; the supplied slices may be shorter, but never longer.
    pub fn process(&mut self, left: &mut [f32], right: &mut [f32]) -> Result<()> {
        anyhow::ensure!(left.len() == right.len(), "stereo block lengths differ");
        anyhow::ensure!(
            left.len() <= self.buffers.inputs[0].capacity(),
            "audio block exceeds prepared VST3 size"
        );
        let frames = left.len();
        // vst3-host renders as many frames as the buffers hold, whatever
        // `block_size` says: at the prepared 8192 a 256-frame quantum cost
        // 8.4 ms instead of 0.2. Within the reserved capacity, resizing
        // never allocates.
        self.buffers.block_size = frames;
        for channel in self.buffers.inputs.iter_mut().chain(self.buffers.outputs.iter_mut()) {
            channel.resize(frames, 0.0);
        }
        self.buffers.inputs[0].copy_from_slice(left);
        self.buffers.inputs[1].copy_from_slice(right);
        self.runner
            .process(&mut self.buffers)
            .context("process VST3 audio block")?;
        left.copy_from_slice(&self.buffers.outputs[0]);
        right.copy_from_slice(&self.buffers.outputs[1]);
        Ok(())
    }
}

fn parameters_of(plugin: &vst3_host::Plugin) -> Result<Vec<Parameter>> {
    Ok(plugin
        .get_parameters()
        .context("read VST3 parameters")?
        .into_iter()
        .filter(|p| p.flags & (HIDDEN | PROGRAM_CHANGE) == 0)
        .map(|p| Parameter {
            id: p.id,
            name: p.name,
            value: p.value,
            default: p.default,
            unit: p.unit,
            steps: p.step_count,
            read_only: p.is_read_only,
            bypass: p.is_bypass,
            list: p.flags & LIST != 0,
        })
        .collect())
}

fn inspect_loaded(plugin: &vst3_host::Plugin, path: &Path) -> Result<PluginInfo> {
    let raw = plugin.info();
    Ok(PluginInfo {
        name: raw.name.clone(),
        vendor: raw.vendor.clone(),
        version: raw.version.clone(),
        category: raw.category.clone(),
        path: path.to_path_buf(),
        parameters: parameters_of(plugin)?,
    })
}

/// Load a plugin in this process and read what it is, without processing.
/// Patch runs this in a child process (`plugins probe`) before a plugin
/// first enters the engine, so a plugin that crashes on load costs nothing.
pub fn inspect(path: &Path, rate: u32, max_frames: usize) -> Result<PluginInfo> {
    let plugin = host(rate, max_frames)?
        .load_plugin(path)
        .with_context(|| format!("load VST3 plugin at {}", path.display()))?;
    inspect_loaded(&plugin, path)
}

/// Load a plugin in a child process (`omarchy-patch plugins probe`) and
/// read it there: a plugin that crashes or hangs on load is refused before
/// it reaches the engine or a window.
pub fn probe_isolated(path: &Path) -> Result<PluginInfo> {
    use std::process::{Command, Stdio};
    let exe = std::env::current_exe().context("find Patch's own binary")?;
    let mut child = Command::new(exe)
        .args(["plugins", "probe"])
        .arg(path)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .context("start the plugin probe")?;
    // Read while it runs: a plugin with many controls writes more than a
    // pipe holds, and the child would wait on us forever.
    let reader = child.stdout.take().map(|mut stdout| {
        std::thread::spawn(move || {
            let mut out = String::new();
            use std::io::Read as _;
            let _ = stdout.read_to_string(&mut out);
            out
        })
    });
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if std::time::Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            anyhow::bail!("{} did not finish loading within 20 s", path.display());
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    };
    let out = reader.and_then(|r| r.join().ok()).unwrap_or_default();
    anyhow::ensure!(
        status.success(),
        "{} failed to load ({status})",
        path.display()
    );
    serde_json::from_str(out.trim()).context("read the plugin probe's answer")
}

/// Ends a process that loaded plugins (probe, host, editor), its answer
/// written. A plugin's library is unloaded with the plugin, and some
/// (MiniMeters) leave exit-time destructors in it: `exit` would call into
/// code no longer mapped and crash, and a probe that answered would read as
/// a plugin that failed. `_exit` runs none of them.
pub fn leave(result: Result<()>) -> ! {
    use std::io::Write as _;
    let code = match result {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("Error: {e:?}");
            1
        }
    };
    let _ = std::io::stdout().flush();
    let _ = std::io::stderr().flush();
    // SAFETY: ends this process; nothing runs after it.
    unsafe { libc::_exit(code) }
}

/// What `plugins probe` does in the child: load, render a block, report.
pub fn probe_here(path: &Path) -> Result<PluginInfo> {
    let mut processor = Processor::load(path, RATE, 1024)?;
    let mut left: Vec<f32> = (0..512)
        .map(|i| (std::f32::consts::TAU * 220.0 * i as f32 / RATE as f32).sin() * 0.1)
        .collect();
    let mut right = left.clone();
    processor.process(&mut left, &mut right)?;
    anyhow::ensure!(
        left.iter().chain(&right).all(|s| s.is_finite()),
        "the plugin returned invalid samples"
    );
    Ok(processor.info().clone())
}

/// A plugin window's own instance of a rack plugin: it never processes
/// audio and shows no editor (the plugin's own editor runs in a process of
/// its own, [`editor`]). It answers what the controls are called and how a
/// value reads ("−3.5 dB").
pub struct Mirror {
    plugin: Arc<Mutex<vst3_host::Plugin>>,
    info: PluginInfo,
}

impl Mirror {
    pub fn load(path: &Path, saved: &[SavedParameter]) -> Result<Self> {
        let mut plugin = host(RATE, 1024)?
            .load_plugin(path)
            .with_context(|| format!("load VST3 plugin at {}", path.display()))?;
        for parameter in saved {
            if check(parameter).is_ok() {
                let _ = plugin.set_parameter(parameter.id, parameter.value);
            }
        }
        let info = inspect_loaded(&plugin, path)?;
        // Restoring is not an edit: start the editor log empty.
        let _ = plugin.get_parameter_changes();
        Ok(Self {
            plugin: Arc::new(Mutex::new(plugin)),
            info,
        })
    }

    pub fn info(&self) -> &PluginInfo {
        &self.info
    }

    fn with<T>(&self, f: impl FnOnce(&mut vst3_host::Plugin) -> T) -> Option<T> {
        self.plugin.lock().ok().map(|mut p| f(&mut p))
    }

    /// The controls with their current values.
    pub fn parameters(&self) -> Vec<Parameter> {
        self.with(|p| parameters_of(p).ok())
            .flatten()
            .unwrap_or_else(|| self.info.parameters.clone())
    }

    /// Set a value from Patch's face (the plugin's editor follows).
    pub fn set(&self, id: u32, value: f64) {
        if value.is_finite() && (0.0..=1.0).contains(&value) {
            self.with(|p| {
                let _ = p.set_parameter(id, value);
                let _ = p.get_parameter_changes();
            });
        }
    }

    /// How the plugin writes a value: "−3.5 dB", "On", "Tape".
    pub fn text(&self, id: u32, value: f64) -> String {
        self.with(|p| p.format_parameter(id, value).ok())
            .flatten()
            .unwrap_or_else(|| format!("{:.0}%", value * 100.0))
    }

    pub fn has_editor(&self) -> bool {
        self.with(|p| p.has_editor()).unwrap_or(false)
    }
}

/// Box Of Bass, the first plugin Patch hosted (its tests use it).
pub fn bundle_path(home: &Path) -> PathBuf {
    home.join(".vst3/Box Of Bass.vst3")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn box_of_bass() -> Option<PathBuf> {
        let path = bundle_path(&PathBuf::from(std::env::var_os("HOME").unwrap_or_default()));
        scan::binary(&path).map(|_| path).or_else(|| {
            eprintln!("Box Of Bass is not installed; skipping the local VST3 test");
            None
        })
    }

    fn tone(frames: usize) -> Vec<f32> {
        (0..frames)
            .map(|i| (std::f32::consts::TAU * 55.0 * i as f32 / 48_000.0).sin() * 0.02)
            .collect()
    }

    #[test]
    fn installed_box_of_bass_processes_a_stereo_block() {
        let Some(path) = box_of_bass() else { return };
        let mut plugin = Processor::load(&path, RATE, 8192).expect("load Box Of Bass");
        let mut left = tone(1024);
        let mut right = left.clone();
        plugin.process(&mut left, &mut right).expect("process stereo block");
        assert!(left.iter().chain(&right).all(|sample| sample.is_finite()));
        assert!(left.iter().chain(&right).any(|sample| sample.abs() > 1.0e-6));
        assert_eq!(plugin.info().name, "Box Of Bass");
    }

    /// The engine's quantum must cost what the plugin's work costs, not
    /// what a block of its largest size would.
    #[test]
    fn a_quantum_costs_a_fraction_of_its_time() {
        let Some(path) = box_of_bass() else { return };
        let mut plugin = Processor::load(&path, RATE, 8192).expect("load Box Of Bass");
        let mut left = tone(256);
        let mut right = left.clone();
        let mut costs: Vec<_> = (0..40)
            .map(|_| {
                let t0 = std::time::Instant::now();
                plugin.process(&mut left, &mut right).expect("process");
                t0.elapsed()
            })
            .collect();
        costs.sort();
        let median = costs[costs.len() / 2];
        // 256 frames are 5.3 ms of audio.
        assert!(median < std::time::Duration::from_millis(2), "{median:?} per quantum");
        // A long block still goes through, in prepared pieces.
        let mut left = tone(4096);
        let mut right = left.clone();
        plugin.process(&mut left, &mut right).expect("process a long block");
        assert!(left.iter().all(|s| s.is_finite()));
    }

    #[test]
    fn a_mirror_reads_values_as_the_plugin_writes_them() {
        let Some(path) = box_of_bass() else { return };
        let mirror = Mirror::load(&path, &[]).expect("mirror Box Of Bass");
        let parameters = mirror.parameters();
        assert!(!parameters.is_empty());
        let first = &parameters[0];
        mirror.set(first.id, 0.5);
        let after = mirror.parameters();
        assert!((after[0].value - 0.5).abs() < 1e-3);
        assert!(!mirror.text(first.id, 0.5).is_empty());
    }
}
