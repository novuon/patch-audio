//! Numbers packed for the wire: i16 little-endian in base64. Large arrays
//! (the Analyzer's scope) cost a fraction of JSON floats to write and read.

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub fn pack_i16(v: &[i16]) -> String {
    let bytes: Vec<u8> = v.iter().flat_map(|x| x.to_le_bytes()).collect();
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for c in bytes.chunks(3) {
        let n = (c[0] as u32) << 16
            | (*c.get(1).unwrap_or(&0) as u32) << 8
            | *c.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            out.push(if i <= c.len() {
                B64[(n >> (18 - 6 * i)) as usize & 63] as char
            } else {
                '='
            });
        }
    }
    out
}

pub fn unpack_i16(s: &str) -> Vec<i16> {
    let val = |c: u8| B64.iter().position(|&b| b == c).map(|p| p as u32);
    let mut bytes = Vec::with_capacity(s.len() / 4 * 3);
    for q in s.as_bytes().chunks(4) {
        let mut n = 0u32;
        let mut k = 0u32;
        for &c in q {
            if let Some(v) = val(c) {
                n = n << 6 | v;
                k += 1;
            }
        }
        n <<= 6 * (4 - k);
        for i in 0..k.saturating_sub(1) {
            bytes.push((n >> (16 - 8 * i)) as u8);
        }
    }
    bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|b| i16::from_le_bytes([b[0], b[1]]))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips() {
        for v in [
            vec![],
            vec![1i16],
            vec![-1, 2],
            vec![i16::MIN, 0, i16::MAX],
            (0..999).map(|i| (i * 37 - 5000) as i16).collect(),
        ] {
            assert_eq!(unpack_i16(&pack_i16(&v)), v);
        }
    }
}
