#!/usr/bin/env python3
"""Play short sounds through Patch's System Sounds and Chromium channels."""

from __future__ import annotations

import math
import shutil
import struct
import subprocess
import sys
import termios
import tty
import wave
from pathlib import Path


SOUND = Path("/usr/share/sounds/freedesktop/stereo/complete.oga")
CACHE_SOUND = (
    Path.home()
    / ".cache"
    / "omarchy-patch"
    / "mixer-sound-test"
    / "complete.oga"
)
SYSTEM_TEST_SOUND = CACHE_SOUND.with_name("system-notification-test.wav")


def system_volume() -> tuple[str, float | None]:
    """Describe the shared Patch output level, when pactl can report it."""
    try:
        result = subprocess.run(
            ["pactl", "get-sink-volume", "patch"],
            check=False,
            capture_output=True,
            text=True,
        )
    except FileNotFoundError:
        return "Patch output level unavailable (pactl is missing).", None
    if result.returncode:
        return "Patch output level unavailable.", None
    lines = result.stdout.splitlines()
    if not lines:
        return "Patch output level unavailable.", None
    first_channel = lines[0].split(",", 1)[0]
    fields = first_channel.split("/")
    if len(fields) >= 3:
        db_text = fields[2].strip().removesuffix(" dB")
        try:
            db = float(db_text)
        except ValueError:
            db = float("-inf") if db_text == "-inf" else None
        return f"Patch shared output: {fields[1].strip()} ({fields[2].strip()}).", db
    return "Patch output level unavailable.", None


def make_system_test_sound() -> None:
    """Write a clear, peak-normalized two-tone ping for the event route."""
    SYSTEM_TEST_SOUND.parent.mkdir(parents=True, exist_ok=True)
    rate = 48_000
    duration = 0.72
    frames = int(rate * duration)
    samples = bytearray()
    for i in range(frames):
        t = i / rate
        # Two short, comfortably faded tones are easier to hear than the
        # quiet desktop completion chime, while staying below full scale.
        if 0.04 <= t < 0.24:
            envelope = max(
                0.0, min((t - 0.04) / 0.015, (0.24 - t) / 0.015, 1.0)
            )
        elif 0.34 <= t < 0.58:
            envelope = max(
                0.0, min((t - 0.34) / 0.015, (0.58 - t) / 0.015, 1.0)
            )
        else:
            envelope = 0.0
        if envelope:
            value = int(
                23_000
                * envelope
                * (
                    math.sin(2 * math.pi * 880 * t)
                    + math.sin(2 * math.pi * 1174.66 * t)
                )
                / 2
            )
        else:
            value = 0
        samples.extend(struct.pack("<hh", value, value))
    with wave.open(str(SYSTEM_TEST_SOUND), "wb") as out:
        out.setnchannels(2)
        out.setsampwidth(2)
        out.setframerate(rate)
        out.writeframes(samples)


def play(*args: str) -> None:
    try:
        result = subprocess.run(["paplay", *args], check=False)
    except FileNotFoundError:
        print("paplay is missing; install pulseaudio-utils.")
        return
    if result.returncode:
        print(f"paplay exited with status {result.returncode}.")


def play_system_sound() -> None:
    make_system_test_sound()
    level, db = system_volume()
    print(f"Playing System Sounds test. {level}", flush=True)
    if db is not None and db <= -20.0:
        print(
            "WARNING: this output level heavily attenuates the test; "
            "raise Patch output to hear it.",
            flush=True,
        )
    # A transient file plus the notification role makes Patch route this
    # generated, unmistakable ping through its System Sounds channel.
    play(
        "--client-name=paplay",
        "--property=media.role=notification",
        str(SYSTEM_TEST_SOUND),
    )


def play_chromium_sound() -> None:
    # A cache copy is outside the sound-theme and transient directories, so
    # Patch treats this as Chromium program audio instead of a UI event.
    CACHE_SOUND.parent.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(SOUND, CACHE_SOUND)
    print("Playing Chromium app-channel test…", flush=True)
    play(
        "--client-name=Chromium",
        "--stream-name=Patch Chromium channel test",
        "--property=media.role=music",
        str(CACHE_SOUND),
    )


def main() -> int:
    if not sys.stdin.isatty():
        print("Run this from an interactive terminal.")
        return 2
    if not SOUND.is_file():
        print(f"Freedesktop test sound not found: {SOUND}")
        return 2

    print("Patch mixer sound check — press a key")
    print("  s  System Sounds notification")
    print("  c  Chromium app stream")
    print("  q  quit")
    print("The System Sounds test is a distinct two-tone ping.")
    print("If it is still silent, the System Sounds route/output is failing;")
    print("the reported Patch output level helps distinguish low level from routing.")

    fd = sys.stdin.fileno()
    old_settings = termios.tcgetattr(fd)
    try:
        tty.setraw(fd)
        while True:
            key = sys.stdin.read(1).lower()
            if key == "q" or key == "\x03":
                break
            if key == "s":
                play_system_sound()
            elif key == "c":
                play_chromium_sound()
    finally:
        termios.tcsetattr(fd, termios.TCSADRAIN, old_settings)
        print("\nDone.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
