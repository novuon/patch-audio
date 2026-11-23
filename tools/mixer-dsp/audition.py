#!/usr/bin/env python3
"""Private, level-matched listening candidates; never changes Patch's live DSP.

uv run --with numpy --with scipy python -I tools/mixer-dsp/audition.py \
    TRACK OUTPUT_DIRECTORY --key PRIVATE_KEY_PATH [--start 90] [--seconds 30]
Run --self-test for filter-response and compressor checks without music.
These are offline prototypes, not a claim of V10 circuit emulation.
"""
from __future__ import annotations

import argparse
import json
import math
from pathlib import Path
import secrets
import subprocess
import tempfile

import numpy as np
from scipy import signal
from scipy.io import wavfile

RATE = 48000


def split(x, hz):
    # LR4: cascaded second-order Butterworth filters, as in Patch's DJ EQ.
    lo = signal.butter(2, hz, fs=RATE, output="sos")
    hi = signal.butter(2, hz, fs=RATE, btype="highpass", output="sos")
    return (signal.sosfilt(np.tile(lo, (2, 1)), x, axis=0),
            signal.sosfilt(np.tile(hi, (2, 1)), x, axis=0))


def phase_match(x, hz):
    a, b = split(x, hz)
    return a + b


def eq_bands(x, four):
    low, rest = split(x, 250)
    mid, high = split(rest, 2500)
    low = phase_match(low, 2500)
    if not four:
        return low, mid, high
    # Split the established middle band. Splitting the entire high branch
    # instead would also change the low/high bands' magnitude responses.
    low_mid, high_mid = split(mid, 1000)
    return (phase_match(low, 1000), low_mid, high_mid,
            phase_match(high, 1000))


def compress(x, amount):
    """Stereo-linked soft-knee candidate: no gate, limiter or auto makeup.

    Threshold -6..-30 dBFS, ratio 1..6:1, 6 dB knee, 10 ms attack,
    120 ms release. Sidechain uses both channels' peak, preserving balance.
    Full bypass is bit exact. This policy requires listening approval.
    """
    amount = float(np.clip(amount, 0, 1))
    if amount == 0:
        return x.copy(), np.zeros(len(x))
    attack, release = math.exp(-1 / (.010 * RATE)), math.exp(-1 / (.120 * RATE))
    peak = np.max(np.abs(x), axis=1)
    env = np.empty(len(x))
    held = 0.0
    for i, value in enumerate(peak):
        coefficient = attack if value > held else release
        held = value + coefficient * (held - value)
        env[i] = held
    level = 20 * np.log10(np.maximum(env, 1e-12))
    above = level - (-6 - 24 * amount)
    slope = 1 - 1 / (1 + 5 * amount)
    reduction = np.where(above <= -3, 0, np.where(above >= 3,
                         slope * above, slope * (above + 3) ** 2 / 12))
    target = 10 ** (-reduction / 20)
    # Smooth gain itself (1 ms), beginning at unity rather than silence.
    coefficient = math.exp(-1 / (.001 * RATE))
    gain, _ = signal.lfilter([1 - coefficient], [1, -coefficient], target,
                             zi=[coefficient])
    return x * gain[:, None], reduction


def self_test():
    impulse = np.zeros((RATE * 2, 2)); impulse[0] = 1
    for four in [False, True]:
        bands = eq_bands(impulse, four)
        response = np.fft.rfft(sum(bands)[:, 0])
        db = 20 * np.log10(np.maximum(np.abs(response), 1e-12))
        frequencies = np.fft.rfftfreq(len(impulse), 1 / RATE)
        error = float(np.max(np.abs(db[(frequencies >= 20) & (frequencies <= 20000)])))
        assert error < .001, error
    # Equal mid gains preserve the old magnitude response; the added split
    # contributes an all-pass phase term, which must also be auditioned.
    three = eq_bands(impulse, False)
    four = eq_bands(impulse, True)
    reference = phase_match(three[0] + three[1] * .5 + three[2], 1000)
    candidate = four[0] + (four[1] + four[2]) * .5 + four[3]
    assert np.max(np.abs(reference - candidate)) < 1e-10
    dc = np.ones((RATE, 2)) * [.5, .125]
    bypass, gr = compress(dc, 0)
    assert np.array_equal(bypass, dc) and np.all(gr == 0)
    wet, gr = compress(dc, 1)
    assert np.isfinite(wet).all() and 18 < gr[-1] < 21
    assert np.max(np.abs(wet[:, 0] / wet[:, 1] - 4)) < 1e-10
    # Predictable steady-state curve: input -6.0206, threshold -30, 6:1.
    expected = (20 * math.log10(.5) + 30) * (1 - 1 / 6)
    assert abs(gr[-1] - expected) < .01
    print("PASS: LR4 flat sum <0.001 dB; equal-mid null <1e-10; "
          "compressor bypass, stereo link and steady-state curve")


def level(path):
    result = subprocess.run(["ffmpeg", "-hide_banner", "-nostats", "-i", str(path),
                             "-af", "loudnorm=I=-18:TP=-1:LRA=11:print_format=json",
                             "-f", "null", "-"], capture_output=True, check=True, text=True)
    report = json.loads(result.stderr[result.stderr.rfind("{"):])
    return float(report["input_i"]), float(report["input_tp"])


def audition(args):
    self_test()
    out, key = Path(args.output).expanduser().resolve(), Path(args.key).expanduser().resolve()
    if key.is_relative_to(out):
        raise ValueError("Keep the answer key outside the listening directory")
    if out.exists() or key.exists():
        raise ValueError("Use new output/key paths; existing listening trials are never overwritten")
    source = Path(args.track).expanduser().resolve(strict=True)
    decoded = subprocess.run(["ffmpeg", "-v", "error", "-ss", str(args.start),
                              "-i", str(source), "-t", str(args.seconds), "-map", "0:a:0",
                              "-ar", str(RATE), "-ac", "2", "-f", "f32le", "pipe:1"],
                             capture_output=True, check=True).stdout
    x = np.frombuffer(decoded, dtype="<f4").astype(np.float64).reshape(-1, 2)
    if len(x) < RATE * 5 or np.max(np.abs(x)) < 1e-6:
        raise ValueError("Need at least five seconds of non-silent stereo music")
    three, four = eq_bands(x, False), eq_bands(x, True)
    cut = 10 ** (-9 / 20)
    old_eq = three[0] + three[1] * cut + three[2]
    new_eq = four[0] + four[1] * cut + four[2] + four[3]
    compressed, gr = compress(x, .65)
    pairs = [("01", [("current broad mid cut", old_eq), ("candidate lower-mid-only cut", new_eq)]),
             ("02", [("uncompressed", x.copy()), ("candidate stereo-linked compression", compressed)])]
    out.mkdir(parents=True)
    secret = {"source": str(source), "start": args.start, "seconds": args.seconds,
              "prototype_only": True, "trials": {}, "compressor_gr_max_db": float(np.max(gr))}
    with tempfile.TemporaryDirectory(prefix="patch-mixer-audition-") as temp:
        for trial, versions in pairs:
            secrets.SystemRandom().shuffle(versions)
            measures = []
            for i, (_, audio) in enumerate(versions):
                fade = min(1200, len(audio) // 2)
                audio[:fade] *= np.linspace(0, 1, fade)[:, None]
                audio[-fade:] *= np.linspace(1, 0, fade)[:, None]
                raw = Path(temp) / f"{trial}-{i}.wav"
                wavfile.write(raw, RATE, audio.astype(np.float32))
                measures.append(level(raw))
            # Constant gains only: no dynamic normalization hiding compressor
            # behavior. Both versions share the same safe integrated target.
            target = min(-18.0, *(-1 - tp + loud for loud, tp in measures))
            for code, (label, audio), (loud, _) in zip(["P", "Q"], versions, measures):
                audio *= 10 ** ((target - loud) / 20)
                path = out / f"{trial}-{code}.wav"
                wavfile.write(path, RATE, audio.astype(np.float32))
                measured = level(path)
                assert abs(measured[0] - target) < .15 and measured[1] <= -.9, measured
                secret["trials"][path.name] = {"version": label, "integrated_lufs": measured[0],
                                                    "true_peak_dbtp": measured[1]}
    (out / "LISTEN.txt").write_text(
        "Patch mixer DSP listening candidates\n\n"
        "These are offline prototypes; live Patch processing is unchanged.\n"
        "Compare 01-P with 01-Q, then 02-P with 02-Q at the same playback level.\n"
        "Each pair is matched in integrated loudness using constant gain.\n"
        "Listen for bass/body separation, vocal clarity, transients and pumping.\n"
        "For each pair report P, Q or tie, plus anything that sounds wrong.\n"
        "Do not infer a version from loudness or filenames; order is randomized.\n"
        "Files and source music are private and must not go in git, releases or kits.\n")
    key.parent.mkdir(parents=True, exist_ok=True)
    with key.open("x") as f:
        key.chmod(0o600)
        json.dump(secret, f, indent=2)
    print(f"Wrote two blind, level-matched pairs to {out}; key kept separately")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("track", nargs="?")
    parser.add_argument("output", nargs="?")
    parser.add_argument("--key")
    parser.add_argument("--start", type=float, default=90)
    parser.add_argument("--seconds", type=float, default=30)
    parser.add_argument("--self-test", action="store_true")
    args = parser.parse_args()
    if args.self_test:
        self_test()
    elif not (args.track and args.output and args.key):
        parser.error("track, output and a separate --key path are required")
    else:
        if not (0 <= args.start and 5 <= args.seconds <= 120):
            parser.error("start must be nonnegative and seconds must be 5..120")
        audition(args)


if __name__ == "__main__":
    main()
