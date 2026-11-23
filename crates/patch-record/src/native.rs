//! Engine-clock program recordings with sample-accurate pause mappings and DJ sidecars.
use crate::daemon::{Take, socket_path};
use crate::wav::WavWriter;
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::time::{Duration, Instant};

#[derive(Default)]
struct State {
    stop: AtomicBool,
    paused: AtomicBool,
    status: Mutex<Value>,
}
struct Cleanup(std::path::PathBuf);
impl Drop for Cleanup {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}
fn io(e: impl ToString) -> std::io::Error {
    std::io::Error::other(e.to_string())
}
fn snapshot() -> Value {
    patch_engine::client::call(&json!({"op":"dj"})).unwrap_or(Value::Null)
}
/// Map a graph sample into a take, accounting for omitted paused intervals.
pub fn recording_sample(segments: &[Value], sample: u64) -> Option<u64> {
    segments.iter().find_map(|s| {
        let start = s["engine_start"].as_u64()?;
        let len = s["frames"].as_u64()?;
        (sample >= start && sample < start + len && s["paused"] != true)
            .then(|| s["recording_start"].as_u64().unwrap_or(0) + sample - start)
    })
}
fn append_segment(
    segments: &mut Vec<Value>,
    sample: u64,
    frames: u64,
    recording: u64,
    paused: bool,
) {
    if let Some(last) = segments.last_mut()
        && last["paused"] == paused
        && last["engine_start"]
            .as_u64()
            .zip(last["frames"].as_u64())
            .is_some_and(|(a, n)| a + n == sample)
    {
        last["frames"] = json!(last["frames"].as_u64().unwrap_or(0) + frames);
        return;
    }
    segments.push(
        json!({"engine_start":sample,"frames":frames,"recording_start":recording,"paused":paused}),
    );
}
fn link_take(take: &Take, state: &Value, complete: bool) {
    if let Some(id) = state["set"]["id"].as_str().filter(|id| !id.is_empty()) {
        let _ = patch_engine::client::call(
            &json!({"op":"link_take","session_id":id,"take":{"path":take.path,"metadata":take.path.with_extension("dj.json"),"complete":complete}}),
        );
    }
}
fn sidecar(
    take: &Take,
    segments: &[Value],
    initial: &Value,
    final_state: &Value,
    frames: u64,
    error: &Option<String>,
) -> Value {
    let session = &final_state["set"];
    let mut transitions = session["transitions"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    for t in &mut transitions {
        t["recording_start"] = json!(
            t["started_sample"]
                .as_u64()
                .and_then(|p| recording_sample(segments, p))
        );
        t["recording_handover"] = json!(
            t["handover_sample"]
                .as_u64()
                .and_then(|p| recording_sample(segments, p))
        );
    }
    json!({"version":1,"take_id":take.path.file_stem().map(|s|s.to_string_lossy()),"path":take.path,"source":"program:mix","format":take.format.label(),"rate":take.rate,"frames":frames,"session_id":session["id"],"name":session["name"],"collections":session["collections"],"initial":initial,"session":session,"transitions":transitions,"segments":segments,"timing":"engine samples; chapter policy is completed crossfader handover","error":error,"complete":error.is_none()})
}
pub fn run(take: Take) -> std::io::Result<()> {
    let sock = socket_path();
    std::fs::create_dir_all(sock.parent().unwrap())?;
    let lock = std::fs::File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(sock.with_extension("lock"))?;
    lock.try_lock()
        .map_err(|e| io(format!("Recorder already running: {e}")))?;
    if UnixStream::connect(&sock).is_ok() {
        return Err(io("Recorder already running"));
    }
    let mut stream = UnixStream::connect(patch_engine::daemon::socket_path())?;
    stream.set_read_timeout(Some(Duration::from_secs(3)))?;
    writeln!(stream, "{}", json!({"op":"capture_program"}))?;
    let initial = snapshot();
    // Confirm the binary stream before opening a take; rate is authoritative.
    let first = patch_engine::capture::read_packet(&mut stream)?;
    if first.rate != take.rate {
        return Err(io(format!(
            "Engine rate is {} Hz; choose that rate for recording",
            first.rate
        )));
    }
    let _ = std::fs::remove_file(&sock);
    let listener = UnixListener::bind(&sock)?;
    listener.set_nonblocking(true)?;
    patch_core::ipc::private_socket(&sock);
    let _cleanup = Cleanup(sock.clone());
    // The bar's icon shows a take running with every window closed.
    let _beat = patch_core::ipc::heartbeat("recording");
    if let Some(dir) = take.path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut wav = WavWriter::create(
        &take.path,
        take.format,
        2,
        take.rate,
        &format!("{} · engine-clock program", take.label),
    )?;
    wav.sync()?;
    let state = Arc::new(State::default());
    *state.status.lock().unwrap() = json!({"recording":true,"paused":false,"source":"program:mix","path":take.path,"label":take.label,"format":take.format.label(),"rate":take.rate,"frames":0,"seconds":0.0,"clips":0,"dropouts":0,"peak":0.0,"level":0.0});
    let control = state.clone();
    let server = std::thread::spawn(move || {
        while !control.stop.load(Ordering::Relaxed) {
            match listener.accept() {
                Ok((mut socket, _)) => {
                    let _ = socket.set_read_timeout(Some(Duration::from_millis(300)));
                    let _ = socket.set_write_timeout(Some(Duration::from_millis(300)));
                    let mut line = String::new();
                    let Ok(read) = socket.try_clone() else {
                        continue;
                    };
                    if BufReader::new(read).read_line(&mut line).is_err() {
                        continue;
                    }
                    match line.trim() {
                        "stop" => {
                            control.stop.store(true, Ordering::Relaxed);
                            let _ = writeln!(socket, "ok");
                        }
                        "pause" => {
                            control.paused.store(true, Ordering::Relaxed);
                            let _ = writeln!(socket, "ok");
                        }
                        "resume" => {
                            control.paused.store(false, Ordering::Relaxed);
                            let _ = writeln!(socket, "ok");
                        }
                        _ => {
                            let mut v = control.status.lock().unwrap().clone();
                            v["paused"] = json!(control.paused.load(Ordering::Relaxed));
                            let _ = writeln!(socket, "{v}");
                        }
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(10))
                }
                Err(_) => {
                    control.stop.store(true, Ordering::Relaxed);
                }
            }
        }
    });
    let reader_state = state.clone();
    let shutdown = stream.try_clone()?;
    let (tx, rx) = mpsc::sync_channel(4096);
    let reader = std::thread::spawn(move || {
        let mut packet = Some(first);
        while !reader_state.stop.load(Ordering::Relaxed) {
            let result = packet
                .take()
                .map(Ok)
                .unwrap_or_else(|| patch_engine::capture::read_packet(&mut stream));
            match result {
                Ok(p) => {
                    if tx
                        .try_send(Ok((p, reader_state.paused.load(Ordering::Relaxed))))
                        .is_err()
                    {
                        reader_state.status.lock().unwrap()["error"] =
                            json!("Disk cannot keep up; take stopped");
                        reader_state.stop.store(true, Ordering::Relaxed);
                        break;
                    }
                }
                Err(e) => {
                    if !reader_state.stop.load(Ordering::Relaxed) {
                        let _ = tx.try_send(Err(format!("Engine program capture ended: {e}")));
                    }
                    break;
                }
            }
        }
    });
    let mut segments = vec![];
    let mut error = None;
    let mut expected = None;
    let mut dither = crate::Dither::tpdf(0x9e3779b9 ^ std::process::id());
    let mut stats = crate::ConvStats::default();
    let mut loud = patch_meter::Loudness::new(take.rate);
    let (mut max_m, mut max_s, mut max_tp) = (f64::NEG_INFINITY, f64::NEG_INFINITY, 0.0f32);
    let mut bytes = Vec::with_capacity(patch_engine::capture::FRAMES * 8);
    let mut sync = Instant::now();
    loop {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(Ok((p, paused))) => {
                if p.rate != take.rate || expected.is_some_and(|next| next != p.sample) {
                    error = Some("Engine clock discontinuity; partial take retained".into());
                    break;
                }
                expected = Some(p.sample + p.frames as u64);
                append_segment(
                    &mut segments,
                    p.sample,
                    p.frames as u64,
                    wav.frames(),
                    paused,
                );
                if !paused {
                    let samples = &p.audio[..p.frames as usize * 2];
                    bytes.clear();
                    if matches!(take.format, crate::wav::SampleFormat::F32) {
                        bytes.extend(samples.iter().flat_map(|v| v.to_le_bytes()));
                        let levels = patch_meter::stats(samples);
                        stats.peak = stats.peak.max(levels.peak[0].max(levels.peak[1]));
                        stats.clips += samples.iter().filter(|v| v.abs() > 1.0).count() as u64;
                    } else {
                        crate::convert_s24(samples, &mut dither, &mut stats, &mut bytes);
                    }
                    if let Err(e) = wav.write(&bytes) {
                        error = Some(format!("Disk write failed: {e}"));
                        break;
                    }
                    loud.push(samples);
                    max_m = max_m.max(loud.momentary());
                    max_s = max_s.max(loud.short_term());
                    let tp = loud.true_peak.take();
                    max_tp = max_tp.max(tp[0].max(tp[1]));
                    let mut status = state.status.lock().unwrap();
                    status["frames"] = json!(wav.frames());
                    status["seconds"] = json!(wav.frames() as f64 / take.rate as f64);
                    status["bytes"] = json!(std::fs::metadata(&take.path).map_or(0, |m| m.len()));
                    status["peak"] = json!(stats.peak);
                    status["level"] = json!(
                        patch_meter::stats(samples)
                            .peak
                            .into_iter()
                            .fold(0.0f32, f32::max)
                    );
                    status["clips"] = json!(stats.clips);
                    status["integrated"] = json!(loud.integrated());
                }
                if sync.elapsed() >= Duration::from_secs(1) {
                    if let Err(e) = wav.sync() {
                        error = Some(format!("Could not sync take: {e}"));
                        break;
                    }
                    link_take(&take, &snapshot(), false);
                    let v = sidecar(
                        &take,
                        &segments,
                        &initial,
                        &snapshot(),
                        wav.frames(),
                        &error,
                    );
                    if let Err(e) = patch_engine::workspace::atomic_json(
                        &take.path.with_extension("dj.json"),
                        &v,
                    ) {
                        error = Some(format!("Could not save DJ metadata: {e}"));
                        break;
                    }
                    sync = Instant::now();
                }
            }
            Ok(Err(e)) => {
                error = Some(e);
                break;
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        if state.stop.load(Ordering::Relaxed) {
            let _ = shutdown.shutdown(std::net::Shutdown::Both);
        }
    }
    state.stop.store(true, Ordering::Relaxed);
    let _ = shutdown.shutdown(std::net::Shutdown::Both);
    let _ = reader.join();
    let _ = server.join();
    if error.is_none() {
        error = state.status.lock().unwrap()["error"]
            .as_str()
            .map(String::from);
    }
    let frames = wav.frames();
    if let Err(e) = wav.finish(Some(crate::wav::BextLoudness {
        integrated: loud.integrated() as f32,
        range: loud.loudness_range() as f32,
        max_true_peak: 20.0 * max_tp.max(1e-8).log10(),
        max_momentary: max_m as f32,
        max_short_term: max_s as f32,
    })) {
        error = Some(format!("Could not finalize take: {e}"));
    }
    let final_state = snapshot();
    let v = sidecar(&take, &segments, &initial, &final_state, frames, &error);
    if let Err(e) = patch_engine::workspace::atomic_json(&take.path.with_extension("dj.json"), &v) {
        error = Some(format!("Could not finalize DJ metadata: {e}"));
    }
    link_take(&take, &final_state, error.is_none());
    let mut result = state.status.lock().unwrap().clone();
    result["recording"] = json!(false);
    result["error"] = json!(error);
    result["metadata"] = json!(take.path.with_extension("dj.json"));
    result["dropouts"] = json!(usize::from(error.is_some()));
    patch_engine::workspace::atomic_json(&sock.with_extension("result.json"), &result)
        .map_err(io)?;
    if let Some(e) = error {
        Err(io(e))
    } else {
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn chapters_map_around_recording_pauses_without_guessing() {
        let mut s = vec![];
        append_segment(&mut s, 100, 200, 0, false);
        append_segment(&mut s, 300, 100, 200, true);
        append_segment(&mut s, 400, 200, 200, false);
        assert_eq!(recording_sample(&s, 150), Some(50));
        assert_eq!(recording_sample(&s, 350), None);
        assert_eq!(recording_sample(&s, 450), Some(250));
        assert_eq!(recording_sample(&s, 600), None);
    }
}
