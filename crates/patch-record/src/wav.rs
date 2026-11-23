//! Crash-safe Broadcast WAV (EBU Tech 3285 v2) that turns into RF64
//! (EBU Tech 3306) when it outgrows 4 GiB.
//!
//! Layout: `RIFF/WAVE | JUNK(28) | bext | fmt (WAVE_FORMAT_EXTENSIBLE) | data`.
//! The JUNK chunk is the reserved space for `ds64`. Sizes are rewritten by
//! [`WavWriter::sync`] (the recorder calls it every second, with fsync), so a
//! file cut short by a crash or power loss is valid up to the last second.

use std::fs::File;
use std::io::{self, Seek, SeekFrom, Write};
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SampleFormat {
    /// IEEE float, 32-bit: bit-exact copy of the PipeWire graph.
    F32,
    /// PCM, 24-bit, TPDF dithered.
    S24,
}

impl SampleFormat {
    pub fn bits(self) -> u16 {
        match self {
            SampleFormat::F32 => 32,
            SampleFormat::S24 => 24,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            SampleFormat::F32 => "32-bit float",
            SampleFormat::S24 => "24-bit PCM · TPDF",
        }
    }
}

/// BWF v2 loudness fields, filled in when the take is finished.
#[derive(Debug, Clone, Copy, Default)]
pub struct BextLoudness {
    pub integrated: f32,
    pub range: f32,
    pub max_true_peak: f32,
    pub max_momentary: f32,
    pub max_short_term: f32,
}

pub struct WavWriter {
    file: File,
    format: SampleFormat,
    channels: u16,
    data_bytes: u64,
    data_start: u64,
    bext_start: u64,
    rf64: bool,
}

const JUNK_LEN: u32 = 28;
const BEXT_LEN: u32 = 602 + 64; // fixed part + coding history
const FMT_LEN: u32 = 40;

fn guid(format: SampleFormat) -> [u8; 16] {
    let tag: u8 = if format == SampleFormat::F32 { 3 } else { 1 };
    [
        tag, 0, 0, 0, 0x00, 0x00, 0x10, 0x00, 0x80, 0x00, 0x00, 0xaa, 0x00, 0x38, 0x9b, 0x71,
    ]
}

fn fixed<const N: usize>(s: &str) -> [u8; N] {
    let mut b = [0u8; N];
    let src = s.as_bytes();
    b[..src.len().min(N)].copy_from_slice(&src[..src.len().min(N)]);
    b
}

/// Local date, time and seconds since local midnight, via `date(1)`.
fn now_local() -> (String, String, u64) {
    let out = std::process::Command::new("date")
        .arg("+%Y-%m-%d %H:%M:%S %H %M %S")
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
    let p: Vec<&str> = out.split_whitespace().collect();
    if p.len() == 5 {
        let secs = p[2].parse::<u64>().unwrap_or(0) * 3600
            + p[3].parse::<u64>().unwrap_or(0) * 60
            + p[4].parse::<u64>().unwrap_or(0);
        (p[0].into(), p[1].into(), secs)
    } else {
        ("0000-00-00".into(), "00:00:00".into(), 0)
    }
}

impl WavWriter {
    pub fn create(
        path: &Path,
        format: SampleFormat,
        channels: u16,
        rate: u32,
        description: &str,
    ) -> io::Result<Self> {
        if channels == 0
            || rate == 0
            || rate
                .checked_mul(channels as u32 * format.bits() as u32 / 8)
                .is_none()
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid WAV rate or channels",
            ));
        }
        let mut file = File::options().write(true).create_new(true).open(path)?;
        let (date, time, since_midnight) = now_local();
        let block = channels as u32 * format.bits() as u32 / 8;
        let mut h: Vec<u8> = vec![];
        h.extend_from_slice(b"RIFF");
        h.extend_from_slice(&0u32.to_le_bytes());
        h.extend_from_slice(b"WAVE");
        // JUNK: room for ds64 if the file grows past 4 GiB.
        h.extend_from_slice(b"JUNK");
        h.extend_from_slice(&JUNK_LEN.to_le_bytes());
        h.extend_from_slice(&[0u8; JUNK_LEN as usize]);
        // bext (Broadcast WAV v2)
        let bext_start = h.len() as u64;
        h.extend_from_slice(b"bext");
        h.extend_from_slice(&BEXT_LEN.to_le_bytes());
        h.extend_from_slice(&fixed::<256>(description));
        h.extend_from_slice(&fixed::<32>("Patch"));
        h.extend_from_slice(&fixed::<32>(&format!("Patch {}", patch_core::VERSION)));
        h.extend_from_slice(&fixed::<10>(&date));
        h.extend_from_slice(&fixed::<8>(&time));
        h.extend_from_slice(&(since_midnight * rate as u64).to_le_bytes()); // TimeReference (samples)
        h.extend_from_slice(&2u16.to_le_bytes()); // version 2
        h.extend_from_slice(&[0u8; 64]); // UMID
        h.extend_from_slice(&[0u8; 10]); // loudness fields, filled by finish()
        h.extend_from_slice(&[0u8; 180]); // reserved
        let history = format!(
            "A=PCM,F={rate},W={},M=stereo,T=Patch {}\r\n",
            format.bits(),
            patch_core::VERSION
        );
        h.extend_from_slice(&fixed::<64>(&history));
        // fmt: WAVE_FORMAT_EXTENSIBLE
        h.extend_from_slice(b"fmt ");
        h.extend_from_slice(&FMT_LEN.to_le_bytes());
        h.extend_from_slice(&0xFFFEu16.to_le_bytes());
        h.extend_from_slice(&channels.to_le_bytes());
        h.extend_from_slice(&rate.to_le_bytes());
        h.extend_from_slice(&(rate * block).to_le_bytes());
        h.extend_from_slice(&(block as u16).to_le_bytes());
        h.extend_from_slice(&format.bits().to_le_bytes());
        h.extend_from_slice(&22u16.to_le_bytes());
        h.extend_from_slice(&format.bits().to_le_bytes()); // valid bits
        h.extend_from_slice(&(if channels == 2 { 3u32 } else { 0 }).to_le_bytes()); // FL|FR
        h.extend_from_slice(&guid(format));
        h.extend_from_slice(b"data");
        h.extend_from_slice(&0u32.to_le_bytes());
        file.write_all(&h)?;
        let data_start = h.len() as u64;
        let mut w = WavWriter {
            file,
            format,
            channels,
            data_bytes: 0,
            data_start,
            bext_start,
            rf64: false,
        };
        w.sync()?;
        Ok(w)
    }

    pub fn write(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.file.write_all(bytes)?;
        self.data_bytes += bytes.len() as u64;
        Ok(())
    }

    pub fn frames(&self) -> u64 {
        self.data_bytes / (self.channels as u64 * self.format.bits() as u64 / 8)
    }

    pub fn bytes(&self) -> u64 {
        self.data_start + self.data_bytes
    }

    /// Rewrite the size fields (switching to RF64 when needed) and flush to
    /// disk. After this returns, the file is complete up to this point.
    pub fn sync(&mut self) -> io::Result<()> {
        let riff = self.data_start - 8 + self.data_bytes + (self.data_bytes & 1);
        if riff > u32::MAX as u64 - 1 && !self.rf64 {
            self.rf64 = true;
        }
        let end = self.file.stream_position()?;
        if self.rf64 {
            self.file.seek(SeekFrom::Start(0))?;
            self.file.write_all(b"RF64")?;
            self.file.write_all(&u32::MAX.to_le_bytes())?;
            self.file.seek(SeekFrom::Start(12))?;
            self.file.write_all(b"ds64")?;
            self.file.write_all(&JUNK_LEN.to_le_bytes())?;
            self.file.write_all(&riff.to_le_bytes())?;
            self.file.write_all(&self.data_bytes.to_le_bytes())?;
            self.file.write_all(&self.frames().to_le_bytes())?;
            self.file.write_all(&0u32.to_le_bytes())?; // table length
            self.file.seek(SeekFrom::Start(self.data_start - 4))?;
            self.file.write_all(&u32::MAX.to_le_bytes())?;
        } else {
            self.file.seek(SeekFrom::Start(4))?;
            self.file.write_all(&(riff as u32).to_le_bytes())?;
            self.file.seek(SeekFrom::Start(self.data_start - 4))?;
            self.file
                .write_all(&(self.data_bytes as u32).to_le_bytes())?;
        }
        self.file.seek(SeekFrom::Start(end))?;
        self.file.sync_data()
    }

    /// Write the take's loudness into `bext` and close the file.
    pub fn finish(mut self, loudness: Option<BextLoudness>) -> io::Result<()> {
        if self.data_bytes & 1 == 1 {
            self.file.write_all(&[0])?; // pad byte
        }
        if let Some(l) = loudness {
            // bext: 8 (id+size) + 256 + 32 + 32 + 10 + 8 + 8 + 2 + 64 = offset 420
            let off = self.bext_start + 8 + 256 + 32 + 32 + 10 + 8 + 8 + 2 + 64;
            let f = |v: f32| -> [u8; 2] {
                if v.is_finite() {
                    ((v * 100.0).round() as i16).to_le_bytes()
                } else {
                    0x7fffu16.to_le_bytes()
                }
            };
            let end = self.file.stream_position()?;
            self.file.seek(SeekFrom::Start(off))?;
            for v in [
                l.integrated,
                l.range,
                l.max_true_peak,
                l.max_momentary,
                l.max_short_term,
            ] {
                self.file.write_all(&f(v))?;
            }
            self.file.seek(SeekFrom::Start(end))?;
        }
        self.sync()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refuses_to_overwrite_and_rejects_invalid_headers() {
        let path = std::env::temp_dir().join(format!("patch-wav-existing-{}", std::process::id()));
        std::fs::write(&path, b"keep this take").unwrap();
        assert_eq!(
            WavWriter::create(&path, SampleFormat::F32, 2, 48_000, "test")
                .err()
                .unwrap()
                .kind(),
            io::ErrorKind::AlreadyExists
        );
        assert_eq!(std::fs::read(&path).unwrap(), b"keep this take");
        assert_eq!(
            WavWriter::create(&path, SampleFormat::F32, 0, 48_000, "test")
                .err()
                .unwrap()
                .kind(),
            io::ErrorKind::InvalidInput
        );
        assert_eq!(
            WavWriter::create(&path, SampleFormat::F32, 2, 0, "test")
                .err()
                .unwrap()
                .kind(),
            io::ErrorKind::InvalidInput
        );
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn writes_a_readable_bwf() {
        let dir = std::env::temp_dir().join(format!("patch-wav-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("t.wav");
        let mut w = WavWriter::create(&p, SampleFormat::S24, 2, 48_000, "test take").unwrap();
        w.write(&[1, 2, 3, 4, 5, 6]).unwrap();
        w.finish(Some(BextLoudness {
            integrated: -23.0,
            range: 5.0,
            max_true_peak: -1.0,
            max_momentary: -20.0,
            max_short_term: -21.0,
        }))
        .unwrap();
        let b = std::fs::read(&p).unwrap();
        assert_eq!(&b[0..4], b"RIFF");
        assert_eq!(&b[8..12], b"WAVE");
        assert_eq!(
            u32::from_le_bytes(b[4..8].try_into().unwrap()) as usize,
            b.len() - 8
        );
        let data = b.windows(4).position(|w| w == b"data").unwrap();
        assert_eq!(
            u32::from_le_bytes(b[data + 4..data + 8].try_into().unwrap()),
            6
        );
        let fmt = b.windows(4).position(|w| w == b"fmt ").unwrap();
        assert_eq!(
            u16::from_le_bytes(b[fmt + 8..fmt + 10].try_into().unwrap()),
            0xFFFE
        );
        let bext = b.windows(4).position(|w| w == b"bext").unwrap();
        let li = i16::from_le_bytes(b[bext + 420..bext + 422].try_into().unwrap());
        assert_eq!(li, -2300);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
