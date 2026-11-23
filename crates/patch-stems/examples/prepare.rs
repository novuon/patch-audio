//! End-to-end background preparation with a configured real separator.
fn main() -> Result<(), String> {
    let source = std::env::args().nth(1).ok_or("Pass an audio path")?;
    let cache = patch_stems::cache::Cache::default_cache()?;
    let (separator, backend) = patch_stems::separator::configured()?;
    let job = patch_stems::jobs::request(
        cache.clone(),
        source.clone().into(),
        source,
        Box::new(separator),
        backend,
        true,
    )?;
    let mut previous = serde_json::Value::Null;
    loop {
        let row = patch_stems::jobs::list(&cache)
            .into_iter()
            .find(|j| j["id"] == job["id"])
            .ok_or("Preparation disappeared")?;
        if previous != row["progress"] {
            println!("{} {}", row["state"], row["progress"]);
            previous = row["progress"].clone();
        }
        match row["state"].as_str() {
            Some("ready") => {
                let lease = cache.acquire(row["set"]["id"].as_str().ok_or("Missing set")?)?;
                if !lease.set.complete() {
                    return Err("Incomplete canonical layout".into());
                }
                println!("{}", serde_json::to_string(&lease.set).unwrap());
                return Ok(());
            }
            Some("failed" | "cancelled" | "interrupted") => return Err(row["error"].to_string()),
            _ => std::thread::sleep(std::time::Duration::from_millis(100)),
        }
    }
}
