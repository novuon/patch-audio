# Mixer DSP listening candidates

This is an **offline prototype**, not the live engine or V10 emulation.
It makes two randomized, level-matched P/Q pairs from a private music file:
a broad middle-band cut versus an independent lower-mid cut, and untreated
versus stereo-linked compression. The four-band candidate retains Patch's
250 Hz / 2.5 kHz outer crossovers and splits the existing mid band at 1 kHz.
Low/high phase compensation keeps the summed magnitude flat. The extra
all-pass phase term must be evaluated along with the useful band separation.

The compressor candidate has a 6 dB soft knee, 10 ms peak-envelope attack,
120 ms release, 1 ms gain smoothing, threshold −6…−30 dBFS and ratio 1…6:1
as the amount rises. It links both stereo channels and adds no automatic
makeup, gate or limiter. Listening copies use constant gain to match their
integrated loudness, with a common true-peak-safe target. Dynamic loudness
normalization is deliberately avoided.

Run the response, bypass, stereo-link and static-curve checks:

```sh
uv run --with numpy --with scipy python -I tools/mixer-dsp/audition.py --self-test
```

Generate a **new** private trial (requires ffmpeg). Output and key paths must
not exist; keep the key outside the listening folder. Music, renders and
keys must stay outside git, releases and tester kits.

```sh
uv run --with numpy --with scipy python -I tools/mixer-dsp/audition.py \
  '/path/to/private/music.flac' \
  "$HOME/Music/Patch Listening Test/Mixer DSP trial" \
  --key "$HOME/.local/state/omarchy-patch/mixer-dsp-trial-key.json"
```

Compare P/Q at the same playback volume and report a preference or tie plus
any pumping, transient loss, harshness or loss of body. Do not open the key
before giving the judgment. Follow `docs/development.md`: adoption waits for
the user's blind listening; a losing design is removed, not left disabled.

After approval, the selected design still needs Rust real-time integration,
control smoothing/transition tests, allocation and CPU measurements at the
supported rates, persistence/IPC/controller coverage and live checks. Changing
the photographed HI MID mapping and activating LOW MID also requires the
user's agreement under the existing Club Booth decisions. The prototype is
not evidence that those controls already work.
