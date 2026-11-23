//! Broadcast telemetry and control, called only by its background poller
//! (the same shape as the Recorder's view: all socket I/O off the UI thread).
use serde_json::{Value, json};

pub fn read(request: &Value) -> Result<Value, String> {
    match request["op"].as_str().unwrap_or("status") {
        "stop" => {
            let result = patch_broadcast::daemon::stop_and_wait()?;
            Ok(json!({"message": "Broadcast stopped.", "status": null, "result": result}))
        }
        "start" => {
            let config = patch_broadcast::daemon::config_from_json(request)?;
            let exe = std::env::current_exe().map_err(|e| e.to_string())?;
            patch_broadcast::daemon::start(&exe, &config)?;
            Ok(json!({
                "op": "start",
                // The state says the rest: CONNECTING until the destination
                // takes the stream, then ON AIR (or FAILED, and why).
                "message": "",
                "status": patch_broadcast::daemon::status(),
            }))
        }
        _ => Ok(match patch_broadcast::daemon::status() {
            Some(status) => status,
            // Not broadcasting. A window following one asks how it ended,
            // so a stream that failed says why instead of going OFFLINE.
            None => json!({
                "offline": true,
                "outcome": if request["outcome"] == true {
                    patch_broadcast::daemon::last_outcome()
                } else {
                    None
                },
            }),
        }),
    }
}
