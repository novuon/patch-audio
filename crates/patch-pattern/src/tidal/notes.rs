//! Notes the way Tidal writes them: `c`, `e5`, `fs4`, `bf` (0 is middle C,
//! the fifth octave), chords as `c'maj`, `e4'min7'ii` and scales by name.
//! The interval tables are standard music theory.

/// "c" → 0, "a4" → -3, "cs" / "c#" → 1, "ef" → 3; a number is itself.
pub fn note_name(s: &str) -> Option<f64> {
    if let Ok(x) = s.parse::<f64>() {
        return x.is_finite().then_some(x);
    }
    let lower = s.to_ascii_lowercase();
    let mut chars = lower.chars().peekable();
    let base = match chars.next()? {
        'c' => 0,
        'd' => 2,
        'e' => 4,
        'f' => 5,
        'g' => 7,
        'a' => 9,
        'b' => 11,
        _ => return None,
    };
    let mut acc = 0;
    while let Some(&c) = chars.peek() {
        match c {
            's' | '#' => acc += 1,
            'f' => acc -= 1,
            'n' => {}
            _ => break,
        }
        chars.next();
    }
    let rest: String = chars.collect();
    let octave: i32 = if rest.is_empty() {
        5
    } else {
        rest.parse().ok()?
    };
    Some(((octave - 5) * 12 + base + acc) as f64)
}

const CHORDS: &[(&[&str], &[i32])] = &[
    (&["major", "maj", "M"], &[0, 4, 7]),
    (&["aug", "plus", "sharp5"], &[0, 4, 8]),
    (&["six", "6", "sixby9", "six9", "sixNine"], &[0, 4, 7, 9]),
    (&["major7", "maj7"], &[0, 4, 7, 11]),
    (&["major9", "maj9"], &[0, 4, 7, 11, 14]),
    (&["add9", "major9add"], &[0, 4, 7, 14]),
    (&["major11", "maj11"], &[0, 4, 7, 11, 14, 17]),
    (&["add11"], &[0, 4, 7, 17]),
    (&["major13", "maj13"], &[0, 4, 7, 11, 14, 21]),
    (&["add13"], &[0, 4, 7, 21]),
    (&["dom7"], &[0, 4, 7, 10]),
    (&["dom9"], &[0, 4, 7, 14]),
    (&["dom11"], &[0, 4, 7, 17]),
    (&["dom13"], &[0, 4, 7, 21]),
    (&["sevenFlat5", "7f5"], &[0, 4, 6, 10]),
    (&["sevenSharp5", "7s5"], &[0, 4, 8, 10]),
    (&["sevenFlat9", "7f9"], &[0, 4, 7, 10, 13]),
    (&["nine"], &[0, 4, 7, 10, 14]),
    (&["eleven", "11"], &[0, 4, 7, 10, 14, 17]),
    (&["thirteen", "13"], &[0, 4, 7, 10, 14, 17, 21]),
    (&["minor", "min", "m"], &[0, 3, 7]),
    (&["diminished", "dim"], &[0, 3, 6]),
    (&["minorSharp5", "msharp5", "mS5"], &[0, 3, 8]),
    (&["minor6", "min6", "m6"], &[0, 3, 7, 9]),
    (&["minorSixNine", "minor69", "min69", "minSixNine", "m69", "mSixNine", "m6by9"], &[0, 3, 9, 7, 14]),
    (&["minor7flat5", "min7flat5", "m7flat5", "m7f5"], &[0, 3, 6, 10]),
    (&["minorMajor7", "minMaj7", "mmaj7"], &[0, 3, 7, 11]),
    (&["diminished7", "dim7"], &[0, 3, 6, 9]),
    (&["minor7", "min7", "m7"], &[0, 3, 7, 10]),
    (&["minor9", "min9", "m9"], &[0, 3, 7, 10, 14]),
    (&["minor11", "min11", "m11"], &[0, 3, 7, 10, 14, 17]),
    (&["minor13", "min13", "m13"], &[0, 3, 7, 10, 14, 17, 21]),
    (&["minorMajor9", "minMaj9", "mmaj9"], &[0, 3, 7, 11, 14]),
    (&["one", "1"], &[0]),
    (&["five", "5"], &[0, 7]),
    (&["sus2"], &[0, 2, 7]),
    (&["sus4"], &[0, 5, 7]),
    (&["sevenSus2", "7sus2"], &[0, 2, 7, 10]),
    (&["sevenSus4", "7sus4"], &[0, 5, 7, 10]),
    (&["nineSus4", "ninesus4", "9sus4"], &[0, 5, 7, 10, 14]),
    (&["sevenFlat10", "7f10"], &[0, 4, 7, 10, 15]),
    (&["sevenSharp5flat9", "7s5f9"], &[0, 4, 8, 10, 13]),
    (&["minor7sharp9", "m7sharp9", "m7s9"], &[0, 3, 7, 10, 14]),
    (&["elevenSharp", "11s"], &[0, 4, 7, 10, 14, 18]),
    (&["minor11sharp", "m11sharp", "m11s"], &[0, 3, 7, 10, 14, 18]),
];

pub fn chord_names() -> impl Iterator<Item = &'static str> {
    CHORDS.iter().map(|(n, _)| n[0])
}

pub fn chord_table(name: &str) -> Option<&'static [i32]> {
    CHORDS
        .iter()
        .find(|(names, _)| names.contains(&name))
        .map(|(_, iv)| *iv)
}

/// `c'maj`, `e4'min7'ii`, `c'major'5`, `a'm9'o`: the notes of a chord.
/// Modifiers: a number (that many notes, climbing octaves), `i` per
/// inversion, `o` open voicing, `d<n>` drop the n-th from the top.
pub fn chord(word: &str) -> Option<Vec<f64>> {
    let mut parts = word.split('\'');
    let root = parts.next()?;
    let root = if root.is_empty() { 0.0 } else { note_name(root)? };
    let name = parts.next()?;
    let base = chord_table(name)?;
    let mut notes: Vec<i32> = base.to_vec();
    for m in parts {
        if m.is_empty() {
            continue;
        }
        if let Ok(n) = m.parse::<usize>() {
            let n = n.clamp(1, 32);
            notes = (0..n)
                .map(|k| base[k % base.len()] + 12 * (k / base.len()) as i32)
                .collect();
        } else if m.chars().all(|c| c == 'i') {
            for _ in 0..m.len() {
                if let Some(first) = notes.first().copied() {
                    notes.remove(0);
                    notes.push(first + 12);
                }
            }
        } else if m == "o" {
            if notes.len() > 2 {
                notes[0] -= 12;
                notes[2] -= 12;
            }
        } else {
            let d = m.strip_prefix('d').and_then(|d| d.parse::<usize>().ok())?;
            if d >= 1 && d <= notes.len() {
                let i = notes.len() - d;
                notes[i] -= 12;
            }
        }
    }
    Some(notes.into_iter().map(|n| root + n as f64).collect())
}

const SCALES: &[(&[&str], &[f64])] = &[
    (&["major", "ionian"], &[0., 2., 4., 5., 7., 9., 11.]),
    (&["minor", "aeolian"], &[0., 2., 3., 5., 7., 8., 10.]),
    (&["dorian"], &[0., 2., 3., 5., 7., 9., 10.]),
    (&["phrygian"], &[0., 1., 3., 5., 7., 8., 10.]),
    (&["lydian"], &[0., 2., 4., 6., 7., 9., 11.]),
    (&["mixolydian"], &[0., 2., 4., 5., 7., 9., 10.]),
    (&["locrian"], &[0., 1., 3., 5., 6., 8., 10.]),
    (&["minPent"], &[0., 3., 5., 7., 10.]),
    (&["majPent"], &[0., 2., 4., 7., 9.]),
    (&["ritusen"], &[0., 2., 5., 7., 9.]),
    (&["egyptian"], &[0., 2., 5., 7., 10.]),
    (&["kumai"], &[0., 2., 3., 7., 9.]),
    (&["hirajoshi"], &[0., 2., 3., 7., 8.]),
    (&["iwato"], &[0., 1., 5., 6., 10.]),
    (&["chinese"], &[0., 4., 6., 7., 11.]),
    (&["indian"], &[0., 4., 5., 7., 10.]),
    (&["pelog"], &[0., 1., 3., 7., 8.]),
    (&["prometheus"], &[0., 2., 4., 6., 11.]),
    (&["scriabin"], &[0., 1., 4., 7., 9.]),
    (&["gong"], &[0., 2., 4., 7., 9.]),
    (&["shang"], &[0., 2., 5., 7., 10.]),
    (&["jiao"], &[0., 3., 5., 8., 10.]),
    (&["zhi"], &[0., 2., 5., 7., 9.]),
    (&["yu"], &[0., 3., 5., 7., 10.]),
    (&["whole", "wholetone"], &[0., 2., 4., 6., 8., 10.]),
    (&["augmented"], &[0., 3., 4., 7., 8., 11.]),
    (&["augmented2"], &[0., 1., 4., 5., 8., 9.]),
    (&["hexMajor7"], &[0., 2., 4., 7., 9., 11.]),
    (&["hexDorian"], &[0., 2., 3., 5., 7., 10.]),
    (&["hexPhrygian"], &[0., 1., 3., 5., 8., 10.]),
    (&["hexSus"], &[0., 2., 5., 7., 9., 10.]),
    (&["hexMajor6"], &[0., 2., 4., 5., 7., 9.]),
    (&["hexAeolian"], &[0., 3., 5., 7., 8., 10.]),
    (&["harmonicMinor"], &[0., 2., 3., 5., 7., 8., 11.]),
    (&["harmonicMajor"], &[0., 2., 4., 5., 7., 8., 11.]),
    (&["melodicMinor"], &[0., 2., 3., 5., 7., 9., 11.]),
    (&["superLocrian"], &[0., 1., 3., 4., 6., 8., 10.]),
    (&["romanianMinor"], &[0., 2., 3., 6., 7., 9., 10.]),
    (&["hungarianMinor"], &[0., 2., 3., 6., 7., 8., 11.]),
    (&["neapolitanMinor"], &[0., 1., 3., 5., 7., 8., 11.]),
    (&["enigmatic"], &[0., 1., 4., 6., 8., 10., 11.]),
    (&["spanish"], &[0., 1., 4., 5., 7., 8., 10.]),
    (&["leadingWhole"], &[0., 2., 4., 6., 8., 10., 11.]),
    (&["lydianMinor"], &[0., 2., 4., 6., 7., 8., 10.]),
    (&["neapolitanMajor"], &[0., 1., 3., 5., 7., 9., 11.]),
    (&["locrianMajor"], &[0., 2., 4., 5., 6., 8., 10.]),
    (&["diminished"], &[0., 2., 3., 5., 6., 8., 9., 11.]),
    (&["diminished2", "octatonic"], &[0., 1., 3., 4., 6., 7., 9., 10.]),
    (&["bhairav"], &[0., 1., 4., 5., 7., 8., 11.]),
    (&["ahirbhairav"], &[0., 1., 4., 5., 7., 9., 10.]),
    (&["todi"], &[0., 1., 3., 6., 7., 8., 11.]),
    (&["purvi"], &[0., 1., 4., 6., 7., 8., 11.]),
    (&["marva"], &[0., 1., 4., 6., 7., 9., 11.]),
    (&["kafi"], &[0., 2., 3., 5., 7., 9., 10.]),
    (&["chromatic"], &[0., 1., 2., 3., 4., 5., 6., 7., 8., 9., 10., 11.]),
];

pub fn scale_names() -> impl Iterator<Item = &'static str> {
    SCALES.iter().map(|(n, _)| n[0])
}

pub fn scale_table(name: &str) -> Option<&'static [f64]> {
    SCALES
        .iter()
        .find(|(names, _)| names.contains(&name))
        .map(|(_, s)| *s)
}

/// The `i`-th degree of a scale, climbing (or falling) octaves past its
/// ends; a fractional degree keeps its fraction.
pub fn degree(steps: &[f64], i: f64) -> f64 {
    let n = steps.len().max(1) as i64;
    let whole = i.floor();
    let frac = i - whole;
    let k = whole as i64;
    let octave = k.div_euclid(n);
    steps[k.rem_euclid(n) as usize] + 12.0 * octave as f64 + frac
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_count_from_middle_c() {
        assert_eq!(note_name("c"), Some(0.0));
        assert_eq!(note_name("c5"), Some(0.0));
        assert_eq!(note_name("a4"), Some(-3.0));
        assert_eq!(note_name("cs"), Some(1.0));
        assert_eq!(note_name("ef6"), Some(15.0));
        assert_eq!(note_name("7"), Some(7.0));
        assert_eq!(note_name("x"), None);
    }

    #[test]
    fn chords_and_their_modifiers() {
        assert_eq!(chord("c'maj"), Some(vec![0.0, 4.0, 7.0]));
        assert_eq!(chord("e'min7"), Some(vec![4.0, 7.0, 11.0, 14.0]));
        assert_eq!(chord("c'maj'ii"), Some(vec![7.0, 12.0, 16.0]));
        assert_eq!(chord("c'major'5"), Some(vec![0.0, 4.0, 7.0, 12.0, 16.0]));
        assert_eq!(chord("c'nope"), None);
    }

    #[test]
    fn degrees_wrap_into_octaves() {
        let major = scale_table("major").unwrap();
        assert_eq!(degree(major, 0.0), 0.0);
        assert_eq!(degree(major, 7.0), 12.0);
        assert_eq!(degree(major, -1.0), -1.0);
        assert_eq!(degree(major, 9.0), 16.0);
    }
}
