//! Numbers for readouts: a value that rounds to zero shows as zero, with no
//! sign. `format!("{:.1}", -0.04)` prints "-0.0" and `{:+.1}` prints "+0.0";
//! both read as an error on a meter or knob (the user, 2026-11-12).

/// `x` to `prec` decimals, never "-0.0".
pub fn fixed(x: impl Into<f64>, prec: usize) -> String {
    let s = format!("{:.prec$}", x.into());
    match s.strip_prefix('-') {
        Some(rest) if is_zero(rest) => rest.to_string(),
        _ => s,
    }
}

/// `x` to `prec` decimals with a "+" on positive values; zero has no sign.
pub fn signed(x: impl Into<f64>, prec: usize) -> String {
    let s = fixed(x, prec);
    if s.starts_with('-') || is_zero(&s) || !s.starts_with(|c: char| c.is_ascii_digit()) {
        s
    } else {
        format!("+{s}")
    }
}

fn is_zero(s: &str) -> bool {
    s.chars().all(|c| c == '0' || c == '.')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_has_no_sign() {
        assert_eq!(fixed(-0.04f32, 1), "0.0");
        assert_eq!(fixed(-0.0f64, 0), "0");
        assert_eq!(fixed(-0.06f32, 1), "-0.1");
        assert_eq!(signed(0.0f32, 1), "0.0");
        assert_eq!(signed(-0.004f64, 2), "0.00");
        assert_eq!(signed(0.04f32, 1), "0.0");
        assert_eq!(signed(1.25f64, 1), "+1.2");
        assert_eq!(signed(-3.0f32, 1), "-3.0");
        assert_eq!(signed(f64::INFINITY, 1), "inf");
        assert_eq!(fixed(f64::NAN, 1), "NaN");
    }
}
