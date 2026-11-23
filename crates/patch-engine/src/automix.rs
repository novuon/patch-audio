//! Not available in this build.

pub fn track_info(
    _path: &str,
    _title: &str,
    _artist: &str,
    _rating: u8,
) -> Option<patch_ai::TrackInfo> {
    None
}

pub fn playing(
    _deck: usize,
    _path: &str,
    _position_s: f64,
    _tempo: f64,
    _key_shift: i32,
) -> Option<patch_ai::Playing> {
    None
}
