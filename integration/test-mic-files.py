#!/usr/bin/env python3
"""Offline Mic calibration/A-B checks. No audio server or device is opened."""
import array
import json
import math
from pathlib import Path
import subprocess
import sys
import tempfile
import wave

binary = str(Path(sys.argv[1]).resolve())
with tempfile.TemporaryDirectory(prefix="patch-mic-files-") as folder:
    root = Path(folder)
    original = root / "original.wav"
    samples = array.array("h", (round((0.001 if i < 12000 else 0.15) * math.sin(2 * math.pi * 200 * i / 48000) * 32768) for i in range(48000)))
    with wave.open(str(original), "wb") as wav:
        wav.setnchannels(1)
        wav.setsampwidth(2)
        wav.setframerate(48000)
        wav.writeframes(samples.tobytes())
    original_bytes = original.read_bytes()

    def run(*args, success=True):
        reply = subprocess.run([binary, "mic", *args], stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=10)
        assert (reply.returncode == 0) == success, reply.stderr.decode()
        return json.loads(reply.stdout) if success else reply

    report = run("calibrate", str(original))
    assert abs(report["peak_dbfs"] + 16.478) < 0.02
    assert report["clipped_samples"] == 0
    assert report["recommended_gate_db"] < -50
    profile = root / "profile.json"
    profile.write_text(json.dumps({"format": "patch-mic", "version": 1, "name": "Offline test", "voice": {"on": False}, "eq": {"bands": [{"on": False, "kind": "bell", "freq": 1000, "gain_db": 0, "q": 1} for _ in range(8)]}}))
    output = root / "processed.wav"
    result = run("compare", str(original), str(profile), str(output))
    assert result["latency_samples"] == 0
    decoded = subprocess.run(["ffmpeg", "-v", "error", "-i", str(output), "-f", "f32le", "-"], check=True, stdout=subprocess.PIPE).stdout
    values = array.array("f")
    values.frombytes(decoded)
    assert len(values) == len(samples)
    assert all(a == b / 32768 for a, b in zip(values, samples)), "bypassed render changed captured samples"
    rendered_bytes = output.read_bytes()
    run("compare", str(original), str(profile), str(output), success=False)
    assert output.read_bytes() == rendered_bytes
    assert original.read_bytes() == original_bytes
    print("Mic files: calibrated levels, quiet-room advice, exact bypass render, duration and overwrite/original protection passed")
