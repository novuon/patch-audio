//! Portable waveform pyramid sidecar (`PWF1`). Values are normalized f32
//! little-endian min/max/RMS triples; storage is independent of the audio
//! decoder and can be mmap'd by waveform renderers.

const MAGIC: &[u8; 4] = b"PWF1";
const HEADER: usize = 24;
const MAX_LEVELS: usize = 16;
const MAX_BINS: usize = 16_000_000;

#[derive(Debug, Clone, PartialEq)]
pub struct Level {
    pub bins: Vec<[f32; 3]>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Waveform {
    pub sample_rate: u32,
    pub channels: u16,
    pub levels: Vec<Level>,
}

impl Waveform {
    pub fn encode(&self) -> Result<Vec<u8>, String> {
        if self.sample_rate == 0
            || self.channels == 0
            || self.levels.is_empty()
            || self.levels.len() > MAX_LEVELS
        {
            return Err("invalid waveform header".into());
        }
        let total: usize = self.levels.iter().map(|l| l.bins.len()).sum();
        if total > MAX_BINS || self.levels.iter().any(|l| l.bins.is_empty()) {
            return Err("waveform bin limit exceeded or empty level".into());
        }
        let mut out = Vec::with_capacity(HEADER + total * 12);
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&self.sample_rate.to_le_bytes());
        out.extend_from_slice(&self.channels.to_le_bytes());
        out.extend_from_slice(&(self.levels.len() as u16).to_le_bytes());
        out.extend_from_slice(&(total as u32).to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        for level in &self.levels {
            out.extend_from_slice(&(level.bins.len() as u32).to_le_bytes());
            for bin in &level.bins {
                if bin
                    .iter()
                    .any(|x| !x.is_finite() || !(-1.0..=1.0).contains(x))
                    || bin[0] > bin[1]
                    || bin[2] < 0.0
                {
                    return Err("invalid waveform bin".into());
                }
                for x in bin {
                    out.extend_from_slice(&x.to_le_bytes());
                }
            }
        }
        let crc = crc32(&out[HEADER..]);
        out[20..24].copy_from_slice(&crc.to_le_bytes());
        Ok(out)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() < HEADER || &bytes[..4] != MAGIC {
            return Err("bad PWF1 magic or truncated header".into());
        }
        let u16at = |p| u16::from_le_bytes([bytes[p], bytes[p + 1]]);
        let u32at = |p| u32::from_le_bytes(bytes[p..p + 4].try_into().unwrap());
        if u16at(4) != 1 || u16at(6) != 0 {
            return Err("unsupported waveform version or flags".into());
        }
        let rate = u32at(8);
        let channels = u16at(12);
        let levels = u16at(14) as usize;
        let declared = u32at(16) as usize;
        let expected_crc = u32::from_le_bytes(bytes[20..24].try_into().unwrap());
        if crc32(&bytes[HEADER..]) != expected_crc {
            return Err("waveform CRC-32 mismatch".into());
        }
        if rate == 0 || channels == 0 || levels == 0 || levels > MAX_LEVELS || declared > MAX_BINS {
            return Err("invalid waveform dimensions".into());
        }
        let mut cursor = HEADER;
        let mut total = 0usize;
        let mut out = Vec::with_capacity(levels);
        for _ in 0..levels {
            if cursor + 4 > bytes.len() {
                return Err("truncated waveform level".into());
            }
            let count = u32at(cursor) as usize;
            cursor += 4;
            total = total
                .checked_add(count)
                .ok_or("waveform bin count overflow")?;
            if count == 0 || total > MAX_BINS {
                return Err("invalid waveform bin count".into());
            }
            let byte_count = count.checked_mul(12).ok_or("waveform size overflow")?;
            if cursor
                .checked_add(byte_count)
                .is_none_or(|end| end > bytes.len())
            {
                return Err("truncated waveform bins".into());
            }
            let mut bins = Vec::with_capacity(count);
            for _ in 0..count {
                let mut triple = [0.0; 3];
                for v in &mut triple {
                    *v = f32::from_le_bytes(bytes[cursor..cursor + 4].try_into().unwrap());
                    cursor += 4;
                }
                if triple
                    .iter()
                    .any(|x| !x.is_finite() || !(-1.0..=1.0).contains(x))
                    || triple[0] > triple[1]
                    || triple[2] < 0.0
                {
                    return Err("invalid waveform bin".into());
                }
                bins.push(triple);
            }
            out.push(Level { bins });
        }
        if total != declared || cursor != bytes.len() {
            return Err("waveform count mismatch or trailing bytes".into());
        }
        Ok(Waveform {
            sample_rate: rate,
            channels,
            levels: out,
        })
    }
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = !0u32;
    for b in bytes {
        crc ^= *b as u32;
        for _ in 0..8 {
            crc = (crc >> 1) ^ if crc & 1 == 1 { 0xedb8_8320 } else { 0 };
        }
    }
    !crc
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn roundtrip_and_reject_corruption() {
        let w = Waveform {
            sample_rate: 44100,
            channels: 2,
            levels: vec![
                Level {
                    bins: vec![[-0.7, 0.8, 0.3], [0.0, 0.5, 0.2]],
                },
                Level {
                    bins: vec![[-0.7, 0.8, 0.4]],
                },
            ],
        };
        let bytes = w.encode().unwrap();
        assert_eq!(Waveform::decode(&bytes).unwrap(), w);
        assert!(Waveform::decode(&bytes[..bytes.len() - 1]).is_err());
        let mut bad = bytes;
        bad[0] = b'X';
        assert!(Waveform::decode(&bad).is_err());
    }
}
