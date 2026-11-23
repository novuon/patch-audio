//! Surface instances.
//!
//! Most surfaces have at most one window. Surfaces marked `multi` (Meters,
//! Library) can have several; each window is an instance with its own id:
//! the plain surface id for the first (`meters`), then `meters#2`,
//! `meters#3`, … The part before `#` selects the surface spec, so layouts,
//! window rules and everything keyed by surface id keep working unchanged.

/// Separator between the surface id and the instance number.
pub const SEP: char = '#';

/// The surface id an instance id belongs to: `meters#2` → `meters`.
pub fn base(id: &str) -> &str {
    id.split_once(SEP).map_or(id, |(b, _)| b)
}

/// Instance number: 1 for the plain id, `n` for `id#n`. `None` unless the
/// suffix is a plain decimal ≥ 2 (`meters#`, `meters#1`, `meters#02` are not
/// instance ids, so every instance has exactly one spelling).
pub fn number(id: &str) -> Option<u32> {
    let Some((_, n)) = id.split_once(SEP) else {
        return Some(1);
    };
    if n.is_empty() || n.starts_with('0') || !n.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    n.parse().ok().filter(|&n| n >= 2)
}

/// The id of instance `n` of `base` (`n <= 1` is the plain id).
pub fn id(base: &str, n: u32) -> String {
    if n <= 1 {
        base.to_string()
    } else {
        format!("{base}{SEP}{n}")
    }
}

/// Is `id` an instance of `base_id`?
pub fn is_of(id: &str, base_id: &str) -> bool {
    base(id) == base_id && number(id).is_some()
}

/// The id a new instance of `base_id` gets, given the open ids: the lowest
/// free number, so closed slots are reused and titles stay short.
pub fn next(base_id: &str, open: &[String]) -> String {
    let taken: Vec<u32> = open
        .iter()
        .filter(|o| is_of(o, base_id))
        .filter_map(|o| number(o))
        .collect();
    let n = (1..).find(|n| !taken.contains(n)).unwrap_or(1);
    id(base_id, n)
}

/// The most recently opened instance of `base_id` (`open` is in opening
/// order): what `close meters` closes.
pub fn newest<'a>(base_id: &str, open: &'a [String]) -> Option<&'a str> {
    open.iter()
        .rev()
        .find(|o| is_of(o, base_id))
        .map(String::as_str)
}

/// The lowest-numbered open instance of `base_id`: what focusing the surface
/// by its key or from the controller brings up.
pub fn first<'a>(base_id: &str, open: &'a [String]) -> Option<&'a str> {
    open.iter()
        .filter(|o| is_of(o, base_id))
        .min_by_key(|o| number(o))
        .map(String::as_str)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parse() {
        assert_eq!(base("meters"), "meters");
        assert_eq!(base("meters#2"), "meters");
        assert_eq!(base("dj-mixer"), "dj-mixer");
        assert_eq!(number("meters"), Some(1));
        assert_eq!(number("meters#2"), Some(2));
        assert_eq!(number("meters#12"), Some(12));
        for bad in [
            "meters#",
            "meters#1",
            "meters#0",
            "meters#x",
            "meters#02",
            "meters#+3",
            "meters#2#3",
        ] {
            assert_eq!(number(bad), None, "{bad}");
        }
        assert_eq!(id("meters", 1), "meters");
        assert_eq!(id("meters", 3), "meters#3");
        assert!(is_of("meters#2", "meters"));
        assert!(!is_of("meters#2", "mixer"));
        assert!(!is_of("meters#x", "meters"));
    }

    #[test]
    fn allocation() {
        assert_eq!(next("meters", &[]), "meters");
        assert_eq!(next("meters", &ids(&["graph", "meters"])), "meters#2");
        assert_eq!(next("meters", &ids(&["meters", "meters#2"])), "meters#3");
        // Closed slots are reused, lowest first.
        assert_eq!(next("meters", &ids(&["meters", "meters#3"])), "meters#2");
        assert_eq!(next("meters", &ids(&["meters#2"])), "meters");
        // Other surfaces don't count.
        assert_eq!(next("library", &ids(&["meters", "meters#2"])), "library");
    }

    #[test]
    fn newest_and_first() {
        let open = ids(&["meters#3", "graph", "meters", "library", "meters#2"]);
        assert_eq!(newest("meters", &open), Some("meters#2"));
        assert_eq!(first("meters", &open), Some("meters"));
        assert_eq!(newest("library", &open), Some("library"));
        assert_eq!(newest("mixer", &open), None);
        assert_eq!(first("mixer", &open), None);
    }
}
