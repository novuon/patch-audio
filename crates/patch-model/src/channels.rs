//! Channel layouts. Patch never assumes stereo: a bus, input or output has a
//! layout of N positioned channels, and stereo is simply N = 2.

/// Speaker/channel position, following PipeWire/SPA names (`FL`, `FR`, …).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Position {
    Mono,
    FL,
    FR,
    FC,
    LFE,
    RL,
    RR,
    SL,
    SR,
    RC,
    TFL,
    TFR,
    TRL,
    TRR,
    /// Unpositioned channel, numbered from 0 (`AUX0` …).
    Aux(u16),
}

impl Position {
    pub fn parse(s: &str) -> Position {
        match s {
            "MONO" => Position::Mono,
            "FL" => Position::FL,
            "FR" => Position::FR,
            "FC" => Position::FC,
            "LFE" => Position::LFE,
            "RL" => Position::RL,
            "RR" => Position::RR,
            "SL" => Position::SL,
            "SR" => Position::SR,
            "RC" => Position::RC,
            "TFL" => Position::TFL,
            "TFR" => Position::TFR,
            "TRL" => Position::TRL,
            "TRR" => Position::TRR,
            aux => Position::Aux(
                aux.strip_prefix("AUX")
                    .and_then(|n| n.parse().ok())
                    .unwrap_or(0),
            ),
        }
    }

    /// Short meter label: "L", "R", "C", "LFE", "Ls", "7" …
    pub fn label(self) -> String {
        match self {
            Position::Mono => "M".into(),
            Position::FL => "L".into(),
            Position::FR => "R".into(),
            Position::FC => "C".into(),
            Position::LFE => "LFE".into(),
            Position::RL => "Lb".into(),
            Position::RR => "Rb".into(),
            Position::SL => "Ls".into(),
            Position::SR => "Rs".into(),
            Position::RC => "Cb".into(),
            Position::TFL => "Ltf".into(),
            Position::TFR => "Rtf".into(),
            Position::TRL => "Ltr".into(),
            Position::TRR => "Rtr".into(),
            Position::Aux(n) => (n + 1).to_string(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChannelLayout {
    pub positions: Vec<Position>,
}

impl Default for ChannelLayout {
    fn default() -> Self {
        ChannelLayout::stereo()
    }
}

impl ChannelLayout {
    pub fn mono() -> Self {
        ChannelLayout {
            positions: vec![Position::Mono],
        }
    }

    pub fn stereo() -> Self {
        ChannelLayout {
            positions: vec![Position::FL, Position::FR],
        }
    }

    /// 5.1 in SMPTE/ITU order (L R C LFE Ls Rs).
    pub fn surround_51() -> Self {
        use Position::*;
        ChannelLayout {
            positions: vec![FL, FR, FC, LFE, SL, SR],
        }
    }

    /// `n` unpositioned channels (an interface's inputs, a multitrack bus).
    pub fn aux(n: usize) -> Self {
        ChannelLayout {
            positions: (0..n as u16).map(Position::Aux).collect(),
        }
    }

    /// From PipeWire's `audio.position`, e.g. `"FL,FR"` or `"[ FL FR FC ]"`.
    pub fn from_pipewire(s: &str) -> Self {
        let positions: Vec<Position> = s
            .split(|c: char| c == ',' || c.is_whitespace() || c == '[' || c == ']')
            .filter(|t| !t.is_empty())
            .map(Position::parse)
            .collect();
        if positions.is_empty() {
            ChannelLayout::stereo()
        } else {
            ChannelLayout { positions }
        }
    }

    pub fn count(&self) -> usize {
        self.positions.len()
    }

    pub fn labels(&self) -> Vec<String> {
        self.positions.iter().map(|p| p.label()).collect()
    }

    /// "mono", "stereo", "5.1", or "8 ch".
    pub fn describe(&self) -> String {
        if *self == ChannelLayout::mono() {
            "mono".into()
        } else if *self == ChannelLayout::stereo() {
            "stereo".into()
        } else if *self == ChannelLayout::surround_51() {
            "5.1".into()
        } else {
            format!("{} ch", self.count())
        }
    }
}

/// Levels of one multichannel signal: per-channel sample peak (dBFS) and,
/// when measured, per-channel VU (0 = reference level).
#[derive(Clone, Debug, PartialEq)]
pub struct Levels {
    pub layout: ChannelLayout,
    pub peak_db: Vec<f32>,
    pub vu: Option<Vec<f32>>,
}

impl Levels {
    pub fn silent(layout: ChannelLayout) -> Self {
        let n = layout.count();
        Levels {
            layout,
            peak_db: vec![crate::audio::SILENCE_DB; n],
            vu: None,
        }
    }

    pub fn stereo(peak_db: [f32; 2], vu: Option<[f32; 2]>) -> Self {
        Levels {
            layout: ChannelLayout::stereo(),
            peak_db: peak_db.to_vec(),
            vu: vu.map(|v| v.to_vec()),
        }
    }

    /// Highest peak across channels.
    pub fn max_db(&self) -> f32 {
        self.peak_db
            .iter()
            .copied()
            .fold(crate::audio::SILENCE_DB, f32::max)
    }
}

impl Default for Levels {
    fn default() -> Self {
        Levels::silent(ChannelLayout::stereo())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layouts() {
        assert_eq!(
            ChannelLayout::from_pipewire("FL,FR"),
            ChannelLayout::stereo()
        );
        assert_eq!(
            ChannelLayout::from_pipewire("[ FL FR FC LFE SL SR ]").describe(),
            "5.1"
        );
        assert_eq!(ChannelLayout::from_pipewire("MONO").describe(), "mono");
        assert_eq!(ChannelLayout::from_pipewire("").count(), 2);
        let aux = ChannelLayout::from_pipewire("AUX0,AUX1,AUX2,AUX3,AUX4,AUX5,AUX6,AUX7");
        assert_eq!(aux, ChannelLayout::aux(8));
        assert_eq!(aux.labels()[7], "8");
        assert_eq!(ChannelLayout::aux(64).describe(), "64 ch");
        assert_eq!(
            ChannelLayout::surround_51().labels(),
            ["L", "R", "C", "LFE", "Ls", "Rs"]
        );
    }

    #[test]
    fn levels() {
        let mut l = Levels::silent(ChannelLayout::aux(8));
        assert_eq!(l.peak_db.len(), 8);
        l.peak_db[5] = -3.0;
        assert_eq!(l.max_db(), -3.0);
    }
}
