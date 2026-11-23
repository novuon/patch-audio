//! Spoken requests to the DJ (the shared types).
use patch_core::intent::Direction;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "intent", rename_all = "snake_case")]
pub enum Intent {
    /// Steer the energy for the next tracks.
    Direction {
        direction: Direction,
    },
    /// Keep the tempo within ± `range` BPM of `bpm`.
    Tempo {
        bpm: f64,
        range: f64,
    },
    /// Where the set's tempo goes from here: faster, slower, each track's
    /// own (see `patch_core::tempo_plan`).
    TempoMode {
        mode: patch_core::tempo_plan::TempoMode,
    },
    /// Pick less predictably among the good fits.
    Surprise,
    /// Stay close to what plays: same key or neighbour, similar energy.
    Similar,
    /// Mix into the next track at the next bar.
    TransitionNow,
    Stems {
        template: crate::stems::Template,
    },
    /// Stop choosing (the music keeps playing).
    Stop,
    /// Loop what plays now, `bars` long (on the bar), until released.
    Loop {
        bars: f32,
    },
    /// Let the loop go: the track plays on.
    LoopRelease,
}

pub const UNDERSTOOD: &str = "energy up / build · keep it steady / hold · more relaxed / calm down · stay around <N> BPM · a bit faster / slower · original tempo · surprise me · something similar · transition now / next · loop this [N bars] / extend · let it go · bass swap / drum introduction / vocal isolation / vocal handoff / acapella over instrumental / gradual stem replacement / phrase stem removal · stop";

/// Parse a request: not available in this build.
pub fn parse(_text: &str) -> Result<Vec<Intent>, String> {
    Err(crate::NOT_IN_THIS_BUILD.into())
}
