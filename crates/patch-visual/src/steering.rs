//! Bounded visual-intent interpretation for choreography; never audio control.
use crate::{
    grid::Grid,
    pack::{Palette, mix},
};
use patch_core::visual_intent::{Colour, Focus, Intent};

#[derive(Default)]
pub struct Steering {
    current: Option<Intent>,
    seconds: f64,
    beats: f64,
    last_seconds: Option<f64>,
    last_beats: Option<f64>,
    previous: Option<Grid>,
}
impl Steering {
    pub fn advance(&mut self, target: Option<Intent>, seconds: f64, beats: f64, dt: f64, bpm: f64) {
        let ds = self
            .last_seconds
            .replace(seconds)
            .map_or(0.0, |v| (seconds - v).max(0.0));
        let db = self
            .last_beats
            .replace(beats)
            .map_or(0.0, |v| (beats - v).max(0.0));
        match target {
            Some(target) => {
                let mut current = self.current.unwrap_or_default();
                let duration = f64::from(target.transition_beats) * 60.0 / bpm.max(60.0);
                let blend = if duration == 0.0 {
                    1.0
                } else {
                    (1.0 - (-dt.max(0.0) * 3.0 / duration).exp()) as f32
                };
                for (a, b) in [
                    (&mut current.intensity, target.intensity),
                    (&mut current.motion, target.motion),
                    (&mut current.density, target.density),
                    (&mut current.symmetry, target.symmetry),
                    (&mut current.feedback, target.feedback),
                ] {
                    *a += (b - *a) * blend;
                }
                current.colour = target.colour;
                current.focus = target.focus;
                current.section = target.section;
                current.hue = target.hue;
                current.transition_beats = target.transition_beats;
                self.current = Some(current);
            }
            None => {
                self.current = None;
                self.previous = None;
            }
        }
        let speed = self.current.map_or(1.0, |i| f64::from(i.motion) * 2.0);
        self.seconds += ds * speed;
        self.beats += db * speed;
    }
    pub fn intent(&self) -> Option<Intent> {
        self.current
    }
    pub fn clocks(&self, seconds: f64, beats: f64) -> (f64, f64) {
        if self.current.is_some() {
            (self.seconds, self.beats)
        } else {
            (seconds, beats)
        }
    }
    pub fn brightness(&self) -> f32 {
        self.current.map_or(1.0, |i| 0.2 + i.intensity * 1.6)
    }
    pub fn energy(&self, whole: f32, bass: f32, mid: f32, high: f32) -> f32 {
        self.current.map_or(whole, |i| match i.focus {
            Focus::Kick | Focus::Bass => bass,
            Focus::Drums => high,
            Focus::Vocals | Focus::Melody => mid,
            Focus::Whole => whole,
        })
    }
    pub fn palette(&self, song: Palette, theme: Palette, beat: f64) -> Palette {
        let Some(i) = self.current else { return song };
        if i.colour == Colour::Theme {
            return theme;
        }
        let hue = i.hue.unwrap_or(0.92)
            + match i.colour {
                Colour::Drift => self.seconds as f32 * 0.015,
                Colour::Step => (beat / 4.0).floor() as f32 * 0.08,
                _ => 0.0,
            };
        let rgb = |h: f32| {
            let h = h.rem_euclid(1.0) * 6.0;
            let x = (1.0 - (h.rem_euclid(2.0) - 1.0).abs()) * 220.0;
            match h as u8 {
                0 => [220, x as u8, 45],
                1 => [x as u8, 220, 45],
                2 => [45, 220, x as u8],
                3 => [45, x as u8, 220],
                4 => [x as u8, 45, 220],
                _ => [220, 45, x as u8],
            }
        };
        Palette {
            name: "intent",
            a: rgb(hue),
            b: rgb(hue + 0.2),
            c: theme.c,
            bg: theme.bg,
        }
    }
    /// Transform only the backdrop: title readability and timing stay intact.
    pub fn backdrop(&mut self, grid: &mut Grid, beat: f64) {
        let Some(i) = self.current else { return };
        for y in 0..grid.rows {
            for x in 0..grid.cols / 2 {
                let left = y * grid.cols + x;
                let right = y * grid.cols + grid.cols - 1 - x;
                let threshold = ((x * 73 + y * 31) % 100) as f32 / 100.0;
                if threshold < i.symmetry {
                    grid.cells[right] = grid.cells[left];
                }
            }
        }
        for (n, cell) in grid.cells.iter_mut().enumerate() {
            if ((n * 73) % 100) as f32 / 100.0 > i.density {
                cell.ch = ' ';
                cell.bg = None;
            }
            if i.colour == Colour::Flash
                && let Some(fg) = cell.fg
            {
                cell.fg = Some(mix(
                    [0, 0, 0],
                    fg,
                    0.35 + 0.65 * (-beat.rem_euclid(1.0) * 5.0).exp() as f32,
                ));
            }
            if let Some(old) = self
                .previous
                .as_ref()
                .filter(|old| old.cols == grid.cols && old.rows == grid.rows)
                && cell.ch == ' '
                && old.cells[n].ch != ' '
                && ((n * 37) % 100) as f32 / 100.0 < i.feedback * 0.7
            {
                *cell = old.cells[n];
                let fg = mix(
                    [0, 0, 0],
                    cell.fg.unwrap_or([194, 199, 195]),
                    i.feedback * 0.8,
                );
                cell.fg = Some(fg);
                cell.bg = cell.bg.map(|bg| mix([0, 0, 0], bg, i.feedback * 0.8));
                if fg.iter().all(|channel| *channel < 3) {
                    cell.ch = ' ';
                    cell.bg = None;
                }
            }
        }
        self.previous = Some(grid.clone());
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sparse_and_mirrored_backdrops_are_bounded() {
        let mut steering = Steering::default();
        let i = Intent {
            density: 1.0,
            symmetry: 1.0,
            feedback: 0.0,
            transition_beats: 0.0,
            ..Intent::default()
        };
        steering.advance(Some(i), 0.0, 0.0, 0.02, 120.0);
        let mut grid = Grid::blank(4, 2);
        grid.cells[0].ch = 'x';
        steering.backdrop(&mut grid, 0.0);
        assert_eq!(grid.cells[3].ch, 'x');
        steering.advance(Some(Intent { density: 0.0, ..i }), 1.0, 2.0, 0.02, 120.0);
        steering.backdrop(&mut grid, 2.0);
        assert!(grid.cells.iter().filter(|c| c.ch != ' ').count() <= 1);
    }
    #[test]
    fn clearing_restores_director_and_never_changes_music() {
        let mut s = Steering::default();
        let i = Intent {
            intensity: 0.0,
            motion: 0.0,
            transition_beats: 0.0,
            ..Intent::default()
        };
        s.advance(Some(i), 1.0, 2.0, 0.02, 120.0);
        assert_eq!(s.brightness(), 0.2);
        s.advance(Some(i), 2.0, 4.0, 0.02, 120.0);
        assert_eq!(s.clocks(2.0, 4.0), (0.0, 0.0));
        s.advance(None, 3.0, 6.0, 0.02, 120.0);
        assert_eq!(s.brightness(), 1.0);
        assert_eq!(s.clocks(3.0, 6.0), (3.0, 6.0));
    }
}
