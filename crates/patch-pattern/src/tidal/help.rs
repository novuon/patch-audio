//! What the Tidal Unit shows as help: each word with its usage, what it
//! does and a line that plays it, and starter blocks. Every example here is
//! evaluated by a test, so the help never shows code that doesn't run.

/// (topic, word, usage, what it does, example)
pub const REFERENCE: &[(&str, &str, &str, &str, &str)] = &[
    // ── Start ──
    ("start", "d1 … d16", "d1 $ pattern", "A channel: evaluating it replaces only that channel. p \"name\" works too.", r#"d1 $ s "bd*4""#),
    ("start", "hush", "hush", "Silence every channel.", "hush"),
    ("start", "silence", "d1 silence", "Silence one channel.", "d1 silence"),
    ("start", "setcps", "setcps (bpm/60/4)", "The tempo, in cycles a second (a cycle is four beats).", "setcps (128/60/4)"),
    ("start", "mute / solo", "mute 1 · solo 2 · unmute 1 · unsolo 2", "Mute or solo channels; unmuteAll, unsoloAll.", "solo 1"),
    ("start", "let", "let name = pattern", "Name a pattern to use in channels.", "let kick = s \"bd*4\""),
    ("start", "let functions", "let name args = …", "Define your own function: let wob n = fast n . rev, then d1 $ wob 2 $ s \"bd sn\".", "let wob n = (# pan sine) . fast n\nd1 $ wob 2 $ s \"bd sn\""),
    ("start", "do", "do d1 $ …\n   d2 $ …", "Several statements as one block, as in Tidal.", "do d1 $ s \"bd*2\"\n   d2 $ s \"~ cp\""),
    ("start", "\\x ->", "(\\x -> …)", "A function written in place: every 2 (\\x -> x # speed 2).", r#"d1 $ every 2 (\x -> x # speed 2 # crush 4) $ s "bd sn""#),
    ("start", "once", "once $ pattern", "Play one cycle of a pattern now, once (a fill, a stab), beside what plays.", r#"once $ s "cp*4" # room 0.4"#),
    ("start", "xfade", "xfade 1 $ … · xfadeIn 1 8 $ …", "Change a channel by crossfading from what it played, over 4 cycles (or as many as you say).", r#"xfadeIn 1 8 $ s "bd*2 [~ sn]""#),
    ("start", "clutch", "clutch 1 $ … · clutchIn 1 4 $ …", "Change a channel event by event: the old pattern's events give way to the new one's at random, over 2 cycles (or as many as you say).", r#"clutchIn 1 4 $ s "hh*8""#),
    ("start", "jumpIn", "jumpIn 1 2 $ … · jumpIn' 1 4 $ …", "Change a channel later: after 2 cycles (jumpIn), or at the next cycle that is a multiple of 4 (jumpIn', jumpMod).", r#"jumpIn' 1 4 $ s "bd(3,8)""#),
    // ── Sound ──
    ("sound", "s", "s \"bd sn\"", "Which sound plays: a sample folder of that name (see Samples), else Patch's drums (bd sn hh oh cp lt mt ht rim cb sh, 808bd …, drum:0–7) or synths (superpiano supersaw supersquare supersine superfm superhammond supermandolin superreese superpad).", r#"d1 $ s "bd [sn cp] hh*2 ~""#),
    ("sound", "n", "n \"0 .. 7\"", "A note number (0 is middle C) or a drum kit's slot; note names (c e g) and chords (c'maj e'min7) too.", r#"d1 $ n "c e g c6" # s "superpiano""#),
    ("sound", "note", "note \"0 7 12\"", "Pitch in semitones: a synth's note, a drum's tuning.", r#"d1 $ s "bd*4" # note "0 -5""#),
    ("sound", "octave", "# octave 4", "The octave notes play in (5 is middle C's).", r#"d1 $ n "0 4 7" # s "superpiano" # octave 4"#),
    ("sound", "gain", "# gain 1.2", "Level (1 as is, 0 silent).", r#"d1 $ s "hh*8" # gain "1 0.7""#),
    ("sound", "pan", "# pan 0.25", "Left 0 … right 1.", r#"d1 $ s "hh*8" # pan sine"#),
    ("sound", "speed", "# speed 2", "Playback rate of a drum (pitch and length).", r#"d1 $ s "bd*4" # speed "1 1.5""#),
    ("sound", "cutoff", "# cutoff 800", "Low-pass filter (Hz); resonance 0 … 1 with it.", r#"d1 $ n "0*8" # s "supersaw" # cutoff (range 300 3000 sine) # resonance 0.3"#),
    ("sound", "hcutoff", "# hcutoff 2000", "High-pass filter (Hz).", r#"d1 $ s "hh*8" # hcutoff 4000"#),
    ("sound", "attack / release", "# attack 0.1 # release 1", "A note's envelope, seconds.", r#"d1 $ n "0 7" # s "superpad" # attack 0.5 # release 2"#),
    ("sound", "legato", "# legato 2", "A synth note's length, in steps (sustain sets seconds).", r#"d1 $ n "0 3 7" # s "supersaw" # legato 0.5"#),
    ("sound", "shape", "# shape 0.6", "Distortion 0 … 1.", r#"d1 $ s "bd*4" # shape 0.5"#),
    ("sound", "crush", "# crush 4", "Bit crushing (fewer bits, harsher).", r#"d1 $ s "sn*4" # crush "<16 4>""#),
    ("sound", "room / size", "# room 0.3 # size 0.8", "Send to the shared reverb, and its size.", r#"d1 $ s "cp" # room 0.4 # size 0.9"#),
    ("sound", "delay", "# delay 0.5 # delaytime 0.25 # delayfeedback 0.5", "Send to the shared delay: time in seconds, feedback.", r#"d1 $ s "rim" # delay 0.6 # delaytime 0.333 # delayfeedback 0.6"#),
    // ── Samples ──
    ("samples", "folders", "~/.local/share/omarchy-patch/samples/<name>/", "A folder of audio files is a sound: s \"name\" plays its first file, name:3 or # n 3 the fourth (in name order, wrapping). SuperDirt's Dirt-Samples are read too. A folder wins over a built-in sound of the same name.", r#"d1 $ s "bd:3 [sn:1 cp]""#),
    ("samples", "begin / end", "# begin 0.25 # end 0.5", "Play part of the sample (fractions of its length).", r#"d1 $ s "break*4" # begin "0 0.25 0.5 0.75" # end "0.25 0.5 0.75 1""#),
    ("samples", "speed", "# speed \"-1\"", "A sample's rate: 2 an octave up and twice as fast, below 0 backwards.", r#"d1 $ s "break" # speed "-1""#),
    ("samples", "unit", "# unit \"c\"", "speed in cycles: speed 1 stretches the sample over a cycle at any tempo.", r#"d1 $ s "break" # unit "c" # speed 1"#),
    ("samples", "accelerate", "# accelerate \"-1\"", "The rate glides by this much while the sample plays.", r#"d1 $ s "arpy*4" # accelerate "-1 1""#),
    ("samples", "cut", "# cut 1", "A cut group: a new sound in it stops the one before (open hats, long loops).", r#"d1 $ s "break*8" # begin (range 0 0.875 saw) # cut 1"#),
    ("samples", "loop", "# loop 2", "Play the part this many times.", r#"d1 $ s "break" # end 0.125 # loop 4"#),
    ("samples", "coarse", "# coarse 8", "Lower the sample rate: every value held this many frames.", r#"d1 $ s "break" # coarse "<1 8 16>""#),
    ("samples", "chop", "chop 8 $ …", "Cut each event into n, each playing its part of the sample in turn: granular when n is large.", r#"d1 $ chop 16 $ s "break" # speed 0.5"#),
    ("samples", "striate", "striate 4 $ …", "The whole pattern n times a cycle, the first part of every sample, then the second …", r#"d1 $ striate 4 $ s "bd sn""#),
    ("samples", "striateBy", "striateBy 8 0.2 $ …", "Like striate, each part this long (they overlap or leave gaps).", r#"d1 $ striateBy 8 0.2 $ s "break""#),
    ("samples", "slice", "slice 8 \"7 6 5 4\" $ …", "Cut the sample into n slices; the pattern says which plays, and when.", r#"d1 $ slice 8 "7 6 5 4 3 2 1 0" $ s "break""#),
    ("samples", "splice", "splice 8 \"0 2 4 6\" $ …", "Slices as slice does, each sped up or slowed to fill its step.", r#"d1 $ splice 8 "0 [2 3] 4 <6 1>" $ s "break""#),
    ("samples", "randslice", "randslice 8 $ …", "A random slice of n for each event.", r#"d1 $ randslice 8 $ s "break*8""#),
    ("samples", "loopAt", "loopAt 2 $ chop 8 $ …", "Fit a sample to n cycles: slower, and stretched by speed (unit \"c\"). With chop it follows tempo changes.", r#"d1 $ loopAt 2 $ chop 8 $ s "break""#),
    // ── Mini-notation ──
    ("mini", "[ ]", "\"bd [sn sn]\"", "A group shares one step.", r#"d1 $ s "bd [sn sn] bd [~ sn]""#),
    ("mini", "< >", "\"<bd sn cp>\"", "One per cycle, in turn.", r#"d1 $ s "bd <sn cp [cp cp]>""#),
    ("mini", "* /", "\"hh*4 bd/2\"", "Faster, slower.", r#"d1 $ s "hh*4 bd/2""#),
    ("mini", "~", "\"bd ~ sn ~\"", "A rest.", r#"d1 $ s "bd ~ sn ~""#),
    ("mini", ",", "\"bd*2, hh*3\"", "Layers at once (polyrhythm).", r#"d1 $ s "bd*2, hh*3""#),
    ("mini", "( , )", "\"bd(3,8)\"", "Euclidean: 3 hits spread over 8 steps (a third number rotates).", r#"d1 $ s "bd(3,8) sn(5,8,2)""#),
    ("mini", "!", "\"bd!3 sn\"", "Repeat a step.", r#"d1 $ s "bd!3 sn""#),
    ("mini", "@ _", "\"bd@3 sn\"", "Longer steps.", r#"d1 $ s "bd@3 sn""#),
    ("mini", "?", "\"hh*8?\"", "Drop events at random (?0.2: a fifth of them).", r#"d1 $ s "hh*16?""#),
    ("mini", "|", "\"bd | cp\"", "One of them each cycle, at random.", r#"d1 $ s "bd*2 [sn | cp]""#),
    ("mini", "{ }%", "\"{bd sn hh}%4\"", "Polymeter: these steps, four to a cycle.", r#"d1 $ s "{bd sn hh}%4""#),
    ("mini", "..", "\"0 .. 7\"", "A run of numbers.", r#"d1 $ n "0 .. 7" # s "superpiano""#),
    ("mini", ":", "\"drum:1\"", "A kit slot (the same as n).", r#"d1 $ s "drum:0 drum:1 drum:2 drum:3""#),
    // ── Time ──
    ("time", "fast", "fast 2 $ p", "Faster (patterned: fast \"<1 2>\").", r#"d1 $ fast "<1 2>" $ s "bd sn""#),
    ("time", "slow", "slow 2 $ p", "Slower.", r#"d1 $ slow 2 $ n "0 2 4 7" # s "superpiano""#),
    ("time", "hurry", "hurry 2 $ p", "Faster and higher (speed too).", r#"d1 $ hurry "<1 2>" $ s "bd sn""#),
    ("time", "<~ ~>", "0.25 <~ p", "Shift earlier or later, in cycles.", r#"d1 $ "<0 0.25>" <~ s "bd sn""#),
    ("time", "rev", "rev $ p", "Each cycle backwards.", r#"d1 $ rev $ n "0 2 4 7" # s "superpiano""#),
    ("time", "iter", "iter 4 $ p", "Start a quarter further in each cycle.", r#"d1 $ iter 4 $ s "bd hh sn cp""#),
    ("time", "palindrome", "palindrome $ p", "Forwards, then backwards.", r#"d1 $ palindrome $ n "0 2 4 7" # s "superpiano""#),
    ("time", "ply", "ply 2 $ p", "Each event repeated.", r#"d1 $ ply "<1 2 3>" $ s "bd sn""#),
    ("time", "press", "press $ p", "Each event pushed to the second half of its step.", r#"d1 $ press $ s "hh*4""#),
    ("time", "swingBy", "swingBy (1/3) 4 $ p", "Swing: every second sixteenth (of 4 slices) later.", r#"d1 $ swingBy (1/3) 4 $ s "hh*8""#),
    ("time", "linger", "linger 0.25 $ p", "Repeat the start of the cycle.", r#"d1 $ linger "<0.25 0.5>" $ s "bd sn hh cp""#),
    ("time", "zoom", "zoom (0.25, 0.75) $ p", "Play part of the cycle, stretched.", r#"d1 $ zoom (0.25, 0.75) $ s "bd sn hh cp""#),
    ("time", "rolledBy", "rolledBy 0.25 $ p", "Strum chords: their notes start one after another over that share of the step (rolled: a quarter; negative: from the top).", r#"d1 $ rolledBy "<0.25 -0.5>" $ n "<c'maj e'min>" # s "superpiano""#),
    ("time", "plyWith", "plyWith 3 (|* gain 0.8) $ p", "Each event repeated, each repeat changed once more.", r#"d1 $ plyWith 3 (|* gain 0.7) $ s "bd sn""#),
    ("time", "bite", "bite 4 \"0 2*2\" $ p", "Cut each cycle into bits and play them in a new order.", r#"d1 $ bite 4 "0 2 [1 3] 2" $ n "0 .. 7" # s "superpiano""#),
    ("time", "chew", "chew 4 \"0 [1 2]\" $ p", "Like bite, and samples speed up or slow down with how much their bit was squeezed.", r#"d1 $ chew 4 "0 [1 2] 3 [2 1]" $ s "bd sn hh cp""#),
    ("time", "fit", "fit 1 [\"bd\", \"sn\"] \"0 1\"", "Pick from a list by number, moving on by the first number each cycle.", r#"d1 $ s (fit 1 ["bd", "sn", "hh", "cp"] "0 [~ 1] 2")"#),
    ("time", "squeeze", "squeeze \"0 1\" [p1, p2]", "Each event plays a whole cycle of the pattern it picks, squeezed into it.", r#"d1 $ squeeze "<0 [0 1]>" [s "bd*4", s "hh cp"]"#),
    ("time", "wedge", "wedge (1/4) p1 p2", "The first quarter of the cycle plays p1, the rest p2.", r#"d1 $ wedge (1/4) (s "bd*2") (s "hh*3")"#),
    ("time", "segment", "segment 8 $ p", "Sample a continuous pattern n times a cycle.", r#"d1 $ n (segment 8 $ range 0 12 sine) # s "supersine""#),
    // ── Conditions ──
    ("change", "every", "every 4 (fast 2) $ p", "Apply a function every n cycles.", r#"d1 $ every 4 (fast 2) $ s "bd sn""#),
    ("change", "whenmod", "whenmod 8 6 rev $ p", "When the cycle mod 8 is 6 or more.", r#"d1 $ whenmod 8 6 rev $ s "bd*2 hh sn""#),
    ("change", "sometimes", "sometimes (# speed 2) $ p", "Apply to half the events, at random (often, rarely, almostNever, almostAlways).", r#"d1 $ sometimes (# speed 2) $ s "hh*8""#),
    ("change", "sometimesBy", "sometimesBy 0.3 f $ p", "Apply to that share of the events.", r#"d1 $ sometimesBy 0.3 (# crush 4) $ s "sn*8""#),
    ("change", "someCycles", "someCycles f $ p", "Apply to whole cycles, at random.", r#"d1 $ someCycles (fast 2) $ s "bd sn""#),
    ("change", "degradeBy", "degradeBy 0.5 $ p", "Drop events at random.", r#"d1 $ degradeBy 0.4 $ s "hh*16""#),
    ("change", "within", "within (0, 0.5) f $ p", "Apply to part of each cycle.", r#"d1 $ within (0, 0.5) (fast 2) $ s "bd sn cp hh""#),
    ("change", "chunk", "chunk 4 f $ p", "Apply to a different quarter each cycle.", r#"d1 $ chunk 4 (hurry 2) $ s "bd sn hh cp""#),
    ("change", "while", "while \"1 0\" f $ p", "Apply where a binary pattern is true.", r#"d1 $ while "1 0" (# speed 2) $ s "hh*8""#),
    ("change", "fix", "fix (fast 2) (s \"hh\") $ p", "Apply only to events with these controls (unfix: to the others; contrast f g: both).", r#"d1 $ fix (# crush 4) (s "sn") $ s "bd sn bd sn""#),
    ("change", "soak", "soak 4 (fast 2 . rev) $ p", "A cycle each of the pattern changed 0, 1, 2 … times.", r#"d1 $ soak 3 (hurry 2) $ s "bd sn""#),
    ("change", "fadeOut", "fadeOut 16 $ p", "Thin out over 16 cycles (fadeIn: fill in).", r#"d1 $ fadeIn 8 $ s "hh*16""#),
    ("change", "select", "select \"<0 0.5>\" [p1, p2]", "A 0 … 1 pattern picks which pattern plays (pickF picks a function).", r#"d1 $ select "<0 0.5>" [s "bd*2", s "cp*3"]"#),
    ("change", "pickF", "pickF \"<0 1>\" [rev, fast 2] $ p", "A number pattern picks the function.", r#"d1 $ pickF "<0 1 2>" [id, rev, fast 2] $ s "bd hh sn""#),
    ("change", "foldEvery", "foldEvery [3, 5] f $ p", "Every 3 and every 5 cycles.", r#"d1 $ foldEvery [3, 5] (fast 2) $ s "bd sn""#),
    // ── Layers ──
    ("layer", "jux", "jux rev $ p", "The pattern left, a changed copy right.", r#"d1 $ jux rev $ s "bd sn hh*2 cp""#),
    ("layer", "off", "off 0.125 (# crush 4) $ p", "A shifted, changed copy on top.", r#"d1 $ off 0.125 (|+ n 12) $ n "0 3 7" # s "supersquare""#),
    ("layer", "superimpose", "superimpose (fast 2) $ p", "A changed copy on top.", r#"d1 $ superimpose (fast 2) $ s "bd sn""#),
    ("layer", "layer", "layer [rev, fast 2] $ p", "Several changed copies at once.", r#"d1 $ layer [id, rev] $ s "bd hh sn""#),
    ("layer", "stut", "stut 3 0.5 (1/8) $ p", "Echoes that fade: count, feedback, time.", r#"d1 $ stut 3 0.5 (1/8) $ s "cp""#),
    ("layer", "echo", "echo 4 0.2 0.5 $ p", "Echoes: count, time, feedback.", r#"d1 $ echo 4 0.2 0.5 $ s "rim""#),
    ("layer", "echoWith", "echoWith 3 (1/8) (|* speed 2) $ p", "Echoes, each changed by a function.", r#"d1 $ echoWith 3 (1/8) (|* speed 1.5) $ s "bd""#),
    ("layer", "stack", "stack [p1, p2]", "Patterns at once.", r#"d1 $ stack [s "bd*2", s "~ cp", s "hh*4"]"#),
    ("layer", "cat", "cat [p1, p2]", "One pattern per cycle, in turn (fastcat: within a cycle).", r#"d1 $ cat [s "bd*2 sn", s "bd sn*2"]"#),
    ("layer", "randcat", "randcat [p1, p2]", "A random one each cycle.", r#"d1 $ randcat [s "bd*2 sn", s "bd ~ sn", s "cp*4"]"#),
    // ── Structure ──
    ("structure", "struct", "struct \"t ~ t t\" $ p", "Take a rhythm from a boolean pattern.", r#"d1 $ struct "t ~ t t" $ s "bd""#),
    ("structure", "mask", "mask \"1 0 1 1\" $ p", "Keep only where true.", r#"d1 $ mask "1 [0 1] 1 1" $ s "hh*16""#),
    ("structure", "euclid", "euclid 3 8 $ p", "Euclidean rhythm (euclidOff, euclidInv, euclidFull too).", r#"d1 $ euclidFull 3 8 (s "bd") (s "hh")"#),
    ("structure", "sew", "sew \"t f\" p1 p2", "Switch between two patterns.", r#"d1 $ sew "t <f t> f" (s "bd*8") (s "cp*8")"#),
    ("structure", "rot", "rot 1 $ p", "Shift the values, keep the rhythm.", r#"d1 $ rot "<0 1 2>" $ n "0 2 4 7" # s "superpiano""#),
    ("structure", "shuffle", "shuffle 4 $ p", "The cycle's quarters in a random order (scramble: with repeats).", r#"d1 $ shuffle 4 $ n "0 .. 7" # s "superpiano""#),
    ("structure", "brak", "brak $ p", "A breakbeat feel: every other cycle squashed and shifted.", r#"d1 $ brak $ s "bd sn""#),
    // ── Harmony ──
    ("harmony", "scale", "scale \"major\" \"0 .. 7\"", "Degrees of a scale (minor, dorian, majPent, minPent, …).", r#"d1 $ n (scale "<major minor>" "0 .. 7") # s "superpiano""#),
    ("harmony", "chords", "n \"c'maj e'min7\"", "Chords by name; 'i inverts, 'o opens, a number sets the count.", r#"d1 $ n "<c'maj e'min a'min7 f'maj7>" # s "superpad""#),
    ("harmony", "arp", "arp \"up\" $ p", "Play chords one note at a time (down, updown, converge, thumbup …).", r#"d1 $ arp "<up downup>" $ n "<c'maj7 a'min9>" # s "supermandolin""#),
    ("harmony", "toScale", "toScale [0, 2, 7] \"0 1 2 3\"", "Degrees of your own scale.", r#"d1 $ n (toScale [0, 3, 5, 7, 10] "0 .. 9") # s "supersaw""#),
    // ── Signals ──
    ("signal", "sine", "# pan sine", "Continuous 0 … 1 (cosine, saw, isaw, tri, square, rand, perlin).", r#"d1 $ s "hh*16" # gain (range 0.6 1.1 sine)"#),
    ("signal", "range", "range 200 2000 sine", "Scale a 0 … 1 pattern.", r#"d1 $ n "0*16" # s "superreese" # cutoff (range 200 2000 $ slow 4 sine)"#),
    ("signal", "irand", "irand 8", "Random whole numbers below n.", r#"d1 $ s "drum*8" # n (irand 8)"#),
    ("signal", "choose", "choose [0, 2, 7]", "A random one of these (with segment for steps).", r#"d1 $ n (segment 8 $ choose [0, 3, 7, 10]) # s "superpiano""#),
    ("signal", "wchoose", "wchoose [(0, 0.25), (2, 0.5)]", "A random one, by weight (wrandcat: a random pattern each cycle, by weight).", r#"d1 $ n (segment 8 $ wchoose [(0, 0.25), (7, 0.5), (12, 0.25)]) # s "superpiano""#),
    ("signal", "binaryN", "binaryN 8 137", "A number's bits as a rhythm (true and false), for struct.", r#"d1 $ struct (binaryN 8 "<137 82>") $ s "bd""#),
    ("signal", "envL", "envL", "0 to 1 over the first cycle, then 1 (envR: 1 to 0).", r#"d1 $ s "hh*8" # gain envL"#),
    ("signal", "run", "run 8", "0 1 2 … 7.", r#"d1 $ n (run 8) # s "superpiano""#),
    // ── Operators ──
    ("operators", "#", "p # gain 1.2", "Add controls; the rhythm comes from the left.", r#"d1 $ s "bd*4" # gain 1.1 # speed 1.2"#),
    ("operators", "|+ |- |* |/", "p |+ n 12", "Arithmetic on controls, rhythm from the left (|+|: both, +|: right).", r#"d1 $ n "0 3 7" |+ n "<0 5>" # s "superpiano""#),
    ("operators", "$", "f $ g $ p", "Apply to everything on the right.", r#"d1 $ every 2 rev $ s "bd sn""#),
    ("operators", ".", "(rev . fast 2)", "Compose functions.", r#"d1 $ every 3 (rev . fast 2) $ s "bd sn hh""#),
    ("operators", "+ -", "n (\"0 2\" + \"<12 7>\")", "Arithmetic on patterns (both rhythms).", r#"d1 $ n ("0 2 4" + "<0 12>") # s "superpiano""#),
];

/// Starter tracks: whole, arranged sessions with one job per channel, so a
/// first run sounds like a record rather than a stack of loops.
pub const STARTERS: &[(&str, &str)] = &[
    ("Night Drive", "setcps (124/60/4)\n\nd1 $ s \"bd*4\"\nd2 $ n \"<0 0 3 5>\" # s \"superreese\" # octave 2 # legato 0.9 # cutoff 500\nd3 $ s \"~ cp ~ cp\" # room 0.25\nd4 $ s \"~ hh ~ hh ~ hh ~ hh\" # hcutoff 6000 # gain 0.7\nd5 $ n \"<a'min7 f'maj7 c'maj7 g'maj7>\" # s \"superpad\" # room 0.5 # gain 0.4\nd6 $ n (scale \"minPent\" \"0 ~ 4 7 ~ 9 7 4\") # s \"superfm\" # octave 4 # delay 0.3 # delaytime 0.375 # gain 0.4"),
    ("Broken Gravity", "setcps (132/60/4)\n\nd1 $ s \"bd(3,8) [~ bd] bd ~\"\nd2 $ n \"0 ~ 0 12 ~ 3 0 7\" # s \"superreese\" # octave 2 # cutoff 700 # legato 0.7\nd3 $ s \"~ ~ cp ~\" # room 0.3 # delay 0.3 # delaytime 0.18 # delayfeedback 0.4\nd4 $ sometimes (# speed 2) $ s \"hh*16?\" # pan sine\nd5 $ jux rev $ n (scale \"minor\" \"0 3 7 10\") # s \"supermandolin\" # octave 4 # delay 0.4 # delaytime 0.25"),
    ("Acid Chapel", "setcps (140/60/4)\n\nd1 $ s \"bd*4\" # shape 0.4\nd2 $ n \"0 ~ 0 12 ~ 3 0 7 10\" # s \"superreese\" # octave 2\n  # cutoff (range 200 2600 $ slow 4 sine) # resonance 0.6 # legato 0.8\nd3 $ s \"~ hh ~ hh\" # hcutoff 5000\nd4 $ every 4 (fast 2) $ s \"~ ~ cp ~\" # room 0.3\nd5 $ arp \"updown\" $ n \"<a'min7 f'maj7>\" # s \"superfm\" # octave 5 # delay 0.4 # delaytime 0.2143"),
    ("Glass Rain", "setcps (118/60/4)\n\nd1 $ slow 2 $ s \"bd(3,8) [~ bd] bd ~\"\nd2 $ iter 4 $ n (scale \"dorian\" \"0 2 4 6 8\") # s \"supermandolin\" # octave 4 # delay 0.5 # delaytime 0.375\nd3 $ juxBy 0.4 rev $ s \"hh*12?\" # pan sine\nd4 $ slow 2 $ n \"<a'min9 f'maj7 d'min9 g'sus4>\" # s \"superpad\" # attack 0.6 # release 2 # room 0.7\nd5 $ degradeBy 0.4 $ n (segment 8 $ scale \"minPent\" $ irand 10) # s \"supersine\" # octave 6 # delay 0.5 # delaytime 0.66"),
    ("Hyperglass", "setcps (150/60/4)\n\nd1 $ s \"bd(7,16) ~ bd(3,16) ~\"\nd2 $ n \"0 3 7 10 12 7 3 0\" # s \"superreese\" # octave 2\n  # cutoff (range 200 3000 $ slow 2 saw) # resonance 0.7 # legato 0.7\nd3 $ stut 3 0.5 (1/16) $ s \"hh*16?\" # pan sine\nd4 $ s \"rim(5,16) cp(3,16)\" # delay 0.5 # delaytime 0.2 # delayfeedback 0.5 # room 0.3\nd5 $ arp \"converge\" $ n \"<a'min7 f'maj7 g'maj7>\" # s \"superfm\" # octave 5 # delay 0.5 # delaytime 0.3"),
    ("Slow Orbit", "setcps (90/60/4)\n\nd1 $ slow 2 $ s \"bd ~ cp ~\" # room 0.3\nd2 $ slow 2 $ n \"<a'min9 f'maj9 c'maj7 g'maj7>\" # s \"superpad\" # attack 0.8 # release 2.5 # room 0.7 # size 0.8\nd3 $ n (segment 8 $ scale \"minPent\" $ irand 10) # s \"supersine\" # octave 6 # delay 0.5 # delaytime 0.66 # delayfeedback 0.5\nd4 $ s \"~ ~ ~ oh\" # room 0.5"),
];

/// The help's topics, in order, with their titles.
pub const TOPICS: &[(&str, &str)] = &[
    ("start", "Start"),
    ("sound", "Sound"),
    ("samples", "Samples"),
    ("mini", "Mini-notation"),
    ("time", "Time"),
    ("change", "Change"),
    ("layer", "Layers"),
    ("structure", "Structure"),
    ("harmony", "Harmony"),
    ("signal", "Signals"),
    ("operators", "Operators"),
];

/// Reference entries matching a search: by word, usage or description.
pub fn search(q: &str) -> Vec<&'static (&'static str, &'static str, &'static str, &'static str, &'static str)> {
    let q = q.trim().to_ascii_lowercase();
    REFERENCE
        .iter()
        .filter(|r| {
            q.is_empty()
                || r.1.to_ascii_lowercase().contains(&q)
                || r.2.to_ascii_lowercase().contains(&q)
                || r.3.to_ascii_lowercase().contains(&q)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tidal::Session;

    #[test]
    fn every_example_runs() {
        for (_, word, _, _, example) in REFERENCE {
            let mut s = Session::default();
            s.apply(example)
                .unwrap_or_else(|e| panic!("{word}: {example}: {}", e[0].message));
        }
        for (name, code) in STARTERS {
            let mut s = Session::default();
            s.apply(code).unwrap_or_else(|e| panic!("{name}: {}", e[0].message));
            let c = s.compile().unwrap();
            let (_, warnings) = c.survey(4);
            assert!(warnings.is_empty(), "{name}: {warnings:?}");
            assert!(!c.events(crate::time::Span::cycles(0, 2)).is_empty(), "{name}");
        }
    }

    #[test]
    fn every_topic_has_entries() {
        for (t, _) in TOPICS {
            assert!(REFERENCE.iter().any(|r| r.0 == *t), "{t}");
        }
        assert!(search("euclid").iter().any(|r| r.1 == "euclid"));
    }
}
