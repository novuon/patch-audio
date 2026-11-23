/// Recency and session membership are independent: a long set can span hours.
pub(super) fn play_recency(last: Option<i64>, now: i64, started: Option<i64>) -> String {
    let Some(last) = last else {
        return "Unplayed".into();
    };
    let age = now.saturating_sub(last).max(0);
    let recent = if age < 3600 {
        "Last hour".into()
    } else if age < 86400 {
        format!("{}h ago", age / 3600)
    } else {
        format!("{}d ago", age / 86400)
    };
    if started.is_some_and(|s| s > 0 && last >= s) {
        format!("This session · {recent}")
    } else {
        recent
    }
}

pub(super) fn played_local_time(at: i64) -> String {
    let stamp = at as libc::time_t;
    let mut local = std::mem::MaybeUninit::<libc::tm>::uninit();
    let mut buf = [0u8; 80];
    // localtime_r writes only to our tm; strftime writes at most buf.len().
    unsafe {
        if libc::localtime_r(&stamp, local.as_mut_ptr()).is_null() {
            return at.to_string();
        }
        let n = libc::strftime(
            buf.as_mut_ptr().cast(),
            buf.len(),
            c"%Y-%m-%d %H:%M:%S %Z".as_ptr(),
            local.as_ptr(),
        );
        if n == 0 {
            at.to_string()
        } else {
            String::from_utf8_lossy(&buf[..n]).into_owned()
        }
    }
}

#[cfg(test)]
mod play_history_tests {
    use super::play_recency;
    #[test]
    fn recency_boundaries_and_long_sessions() {
        assert_eq!(play_recency(None, 100000, None), "Unplayed");
        assert_eq!(play_recency(Some(99999), 100000, None), "Last hour");
        assert_eq!(play_recency(Some(96400), 100000, None), "1h ago");
        assert_eq!(play_recency(Some(13600), 100000, None), "1d ago");
        assert_eq!(
            play_recency(Some(90000), 100000, Some(80000)),
            "This session · 2h ago"
        );
        assert_eq!(play_recency(Some(79000), 100000, Some(80000)), "5h ago");
    }
}
