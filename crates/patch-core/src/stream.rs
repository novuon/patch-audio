//! Native playback lanes. Identity belongs to one exposed stream, never to
//! an application's name or executable (several web apps can share both).
pub const CHANNELS: [&str; 32] = [
    "stream-1",
    "stream-2",
    "stream-3",
    "stream-4",
    "stream-5",
    "stream-6",
    "stream-7",
    "stream-8",
    "stream-9",
    "stream-10",
    "stream-11",
    "stream-12",
    "stream-13",
    "stream-14",
    "stream-15",
    "stream-16",
    "stream-17",
    "stream-18",
    "stream-19",
    "stream-20",
    "stream-21",
    "stream-22",
    "stream-23",
    "stream-24",
    "stream-25",
    "stream-26",
    "stream-27",
    "stream-28",
    "stream-29",
    "stream-30",
    "stream-31",
    "stream-32",
];

pub fn key(serial: u32, client: Option<u32>, process: &str) -> String {
    format!(
        "stream:{serial}:{}:{process}",
        client.map(|c| c.to_string()).unwrap_or_else(|| "-".into())
    )
}

pub fn is_key(value: &str) -> bool {
    let Some(rest) = value.strip_prefix("stream:") else {
        return false;
    };
    let parts: Vec<_> = rest.split(':').collect();
    parts.len() == 3
        && parts[0].parse::<u32>().is_ok()
        && (parts[1] == "-" || parts[1].parse::<u32>().is_ok())
        && (parts[2].is_empty() || parts[2].parse::<u32>().is_ok())
}
