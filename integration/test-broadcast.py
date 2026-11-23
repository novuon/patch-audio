#!/usr/bin/env python3
"""Broadcast Unit end to end, with no audio server and no network.

A fake `parec` (a paced 48 kHz stereo tone) stands in for PipeWire and local
fake Icecast servers stand in for real ones; the real broadcast process and
the real ffmpeg do the rest, in a private runtime folder. Needs ffmpeg and
ffprobe. Each check here is a bug the Unit once had (2026-11-22 audit).
"""
import base64
import json
import os
from pathlib import Path
import socket
import struct
import subprocess
import sys
import tempfile
import threading
import time

binary = str(Path(sys.argv[1] if len(sys.argv) > 1 else "target/release/omarchy-patch").resolve())

FEED = r'''
import math, os, struct, sys, time
sr = 48000
chunk = b"".join(struct.pack("<ff", *(2 * [0.3 * math.sin(2 * math.pi * 440 * i / sr)])) for i in range(960))
gaps = [tuple(map(float, g.split("-"))) for g in os.environ.get("FAKE_PAREC_GAPS", "").split(",") if g]
t0 = time.monotonic()
try:
    for k in range(600 * 50):
        time.sleep(max(0, t0 + k * 0.02 - time.monotonic()))
        if any(a <= k * 0.02 < b for a, b in gaps):
            continue
        sys.stdout.buffer.write(chunk)
        sys.stdout.buffer.flush()
except BrokenPipeError:
    pass
'''


class FakeIcecast:
    """One local Icecast stand-in. `script` is a list of connections, each
    (answer, delay, hold): "ok" (100 Continue after `delay`, then take audio
    for `hold` s and hang up), "401" (refuse after `delay`) or "stall" (say
    nothing for `hold` s)."""

    def __init__(self, script):
        self.srv = socket.socket()
        self.srv.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        self.srv.bind(("127.0.0.1", 0))
        self.srv.listen(4)
        self.srv.settimeout(30)
        self.port = self.srv.getsockname()[1]
        self.auth, self.bytes = [], 0
        self.thread = threading.Thread(target=self.serve, args=(script,), daemon=True)
        self.thread.start()

    def serve(self, script):
        for answer, delay, hold in script:
            try:
                c, _ = self.srv.accept()
            except OSError:
                return
            data = b""
            while b"\r\n\r\n" not in data:
                d = c.recv(4096)
                if not d:
                    break
                data += d
            for h in data.split(b"\r\n\r\n")[0].decode(errors="replace").split("\r\n"):
                if h.lower().startswith("authorization: basic "):
                    self.auth.append(base64.b64decode(h.split()[-1]).decode())
            time.sleep(delay)
            if answer == "401":
                c.sendall(b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\n\r\n")
                time.sleep(0.5)
            elif answer == "ok":
                c.sendall(b"HTTP/1.1 100 Continue\r\n\r\n")
                c.settimeout(0.5)
                until = time.monotonic() + hold
                while time.monotonic() < until:
                    try:
                        d = c.recv(65536)
                    except socket.timeout:
                        continue
                    if not d:
                        break
                    self.bytes += len(d)
            else:
                time.sleep(hold)
            c.close()


with tempfile.TemporaryDirectory(prefix="patch-bc-") as folder:
    root = Path(folder)
    (root / "run").mkdir(mode=0o700)
    (root / "bin").mkdir()
    (root / "feed.py").write_text(FEED)
    parec = root / "bin" / "parec"
    parec.write_text(f"#!/bin/sh\nexec python3 -I {root / 'feed.py'}\n")
    parec.chmod(0o755)
    env = dict(os.environ, PATH=f"{root / 'bin'}:{os.environ['PATH']}", XDG_RUNTIME_DIR=str(root / "run"))
    sock = root / "run" / "omarchy-patch" / "broadcast.sock"

    def cli(*args, timeout=30, extra=None):
        return subprocess.run(
            [binary, "broadcast", *args], env=dict(env, **(extra or {})), timeout=timeout,
            stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)

    def status():
        out = cli("status").stdout
        return json.loads(out) if out.lstrip().startswith("{") else out.strip()

    def stop():
        began = time.monotonic()
        r = cli("stop")
        assert r.returncode == 0, r.stderr
        return json.loads(r.stdout), time.monotonic() - began

    def wait_for(pred, secs, what):
        until = time.monotonic() + secs
        while time.monotonic() < until:
            s = status()
            if pred(s):
                return s
            time.sleep(0.2)
        raise AssertionError(f"{what}: last status {s}")

    # 1. A file: finalized on stop; a second one never overwrites the first.
    path = root / "sets" / "set.ogg"
    r = cli("start", "file", str(path))
    assert r.returncode == 0 and r.stdout.startswith("broadcasting to"), (r.stdout, r.stderr)
    time.sleep(3)
    result, took = stop()
    assert result["error"] is None and result["seconds"] > 2.5, result
    duration = float(subprocess.run(
        ["ffprobe", "-v", "error", "-show_entries", "format=duration", "-of", "csv=p=0", str(path)],
        check=True, stdout=subprocess.PIPE, text=True).stdout)
    assert abs(duration - result["seconds"]) < 0.3, (duration, result)
    data = path.read_bytes()
    assert data[data.rfind(b"OggS") + 5] & 4, "the Ogg file has no end-of-stream page"
    first = path.read_bytes()
    r = cli("start", "file", str(path))
    assert r.returncode == 0, r.stderr
    assert status()["target"].endswith("set-2.ogg")
    stop()
    assert path.read_bytes() == first and (root / "sets" / "set-2.ogg").stat().st_size > 0

    # 2. Icecast: CONNECTING until the server takes the stream, then ON AIR
    #    with real bytes; a password any character can be in arrives whole.
    server = FakeIcecast([("ok", 1.5, 20)])
    proc = subprocess.Popen(
        [binary, "broadcast", "start", "icecast", f"http://127.0.0.1:{server.port}", "--mount", "patch",
         "--password", "p@ss/w#rd?:%x"], env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    wait_for(lambda s: isinstance(s, dict), 3, "the broadcast process")
    s = status()
    assert s["state"] == "connecting", f"ON AIR before the server took the stream: {s}"
    s = wait_for(lambda s: isinstance(s, dict) and s["state"] == "live", 5, "ON AIR")
    out, err = proc.communicate(timeout=15)
    assert proc.returncode == 0 and out.startswith("broadcasting to"), (out, err)
    assert server.auth == ["source:p@ss/w#rd?:%x"], server.auth
    s = wait_for(lambda s: isinstance(s, dict) and s["bytes"] > 0, 3, "bytes sent")
    assert "p@ss" not in json.dumps(s)
    stop()

    # 3. A server that drops the stream: RECONNECTING, then ON AIR again,
    #    one outage counted, retried after 1 s.
    server = FakeIcecast([("ok", 0, 3), ("ok", 0, 20)])
    r = cli("start", "icecast", f"http://127.0.0.1:{server.port}", "--mount", "patch", "--user", "source",
            "--password", "hunter2")
    assert r.returncode == 0, r.stderr
    wait_for(lambda s: isinstance(s, dict) and s["state"] == "reconnecting", 15, "RECONNECTING")
    s = wait_for(lambda s: isinstance(s, dict) and s["state"] == "live", 6, "ON AIR again")
    assert s["reconnects"] == 1 and s["dropouts"] == 1, s
    assert "hunter2" not in json.dumps(s)
    stop()

    # 4. A refusal: start says why, status still says why afterwards, and
    #    the password is nowhere in either.
    server = FakeIcecast([("401", 1.0, 0)])
    r = cli("start", "icecast", f"http://127.0.0.1:{server.port}", "--mount", "patch", "--user", "source",
            "--password", "hunter2")
    assert r.returncode != 0 and "401" in r.stderr, (r.stdout, r.stderr)
    s = status()
    assert isinstance(s, str) and s.startswith("not broadcasting · the last broadcast failed") and "401" in s, s
    assert "hunter2" not in r.stderr + s

    # 5. A server that takes the connection and never answers: never ON AIR,
    #    and stop ends it all within seconds (it once hung for as long as
    #    the server held on, while status said "not broadcasting").
    server = FakeIcecast([("stall", 0, 60)])
    proc = subprocess.Popen(
        [binary, "broadcast", "start", "icecast", f"http://127.0.0.1:{server.port}", "--mount", "patch"],
        env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    wait_for(lambda s: isinstance(s, dict), 3, "the broadcast process")
    time.sleep(2)
    assert status()["state"] == "connecting"
    result, took = stop()
    assert took < 8, f"stop took {took:.1f} s"
    assert not sock.exists()
    proc.communicate(timeout=10)

    # 6. Two 1 s gaps in the capture count twice (the count used to stick
    #    after the first; a slow start counted one).
    r = cli("start", "file", str(root / "sets" / "gaps.ogg"), extra={"FAKE_PAREC_GAPS": "3-4,7-8"})
    assert r.returncode == 0, r.stderr
    time.sleep(10)
    assert status()["dropouts"] == 2, status()
    stop()

    # 7. RTMP carries the picture: a window's frames, paced onto the sound
    #    (H.264 1280×720 at 30 fps with the AAC, in step), black when no
    #    window sends, and the stream key nowhere to be seen.
    free_port = socket.socket()
    free_port.bind(("127.0.0.1", 0))
    port = free_port.getsockname()[1]
    free_port.close()
    received = root / "received.flv"
    server = subprocess.Popen(
        ["ffmpeg", "-hide_banner", "-loglevel", "error", "-listen", "1", "-timeout", "30",
         "-i", f"rtmp://127.0.0.1:{port}/live", "-c", "copy", "-y", str(received)],
        stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
    time.sleep(0.5)
    feeding = threading.Event()
    feeding.set()
    color = bytes([200, 40, 40, 255]) * (1280 * 720)

    def window():
        """A Patch window sending its picture, 30 frames a second, while `feeding`."""
        path = root / "run" / "omarchy-patch" / "broadcast.picture.sock"
        conn = None
        t0 = time.monotonic()
        k = 0
        while feeding.is_set():
            if conn is None:
                try:
                    conn = socket.socket(socket.AF_UNIX)
                    conn.connect(str(path))
                    conn.sendall(b"PATCHVF1" + struct.pack("<II", 1280, 720))
                except OSError:
                    conn = None
                    time.sleep(0.05)
                    continue
            conn.sendall(color)
            k += 1
            time.sleep(max(0, t0 + k / 30 - time.monotonic()))
        if conn:
            conn.close()

    sender = threading.Thread(target=window, daemon=True)
    sender.start()
    r = cli("start", "rtmp", f"rtmp://127.0.0.1:{port}/live", "--key", "SECRETKEY",
            extra={"PATCH_BROADCAST_H264": "libx264"})
    assert r.returncode == 0 and r.stdout.startswith("broadcasting to"), (r.stdout, r.stderr)
    s = wait_for(lambda s: isinstance(s, dict) and s["picture_live"], 5, "the picture on air")
    assert s["picture"] == "performer" and s["video_encoder"] == "libx264", s
    assert s["target"] == f"127.0.0.1:{port}/live/••••" and "SECRETKEY" not in json.dumps(s), s
    time.sleep(3)
    feeding.clear()
    sender.join()
    wait_for(lambda s: isinstance(s, dict) and not s["picture_live"], 4, "no picture without a window")
    time.sleep(1.5)
    result, took = stop()
    assert result["error"] is None, result
    server.wait(timeout=15)

    def probe(*args):
        return subprocess.run(["ffprobe", "-v", "error", *args, str(received)],
                              check=True, stdout=subprocess.PIPE, text=True).stdout.split()
    streams = probe("-show_entries", "stream=codec_name,width,height,r_frame_rate", "-of", "csv=p=0")
    assert "h264,1280,720,30/1" in streams and any(s.startswith("aac") for s in streams), streams
    last = {kind: float(probe("-select_streams", kind, "-show_entries", "packet=pts_time", "-of", "csv=p=0")[-1])
            for kind in ("v", "a")}
    assert abs(last["v"] - last["a"]) < 0.25, f"picture and sound out of step: {last}"

    def pixel(at):
        raw = subprocess.run(["ffmpeg", "-v", "error", "-ss", str(at), "-i", str(received), "-frames:v", "1",
                              "-f", "rawvideo", "-pix_fmt", "rgb24", "-"], check=True, stdout=subprocess.PIPE).stdout
        i = (360 * 1280 + 640) * 3
        return list(raw[i:i + 3])
    lit, dark = pixel(1.5), pixel(last["v"] - 0.3)
    assert all(abs(a - b) <= 12 for a, b in zip(lit, [200, 40, 40])), f"the window's colour came out as {lit}"
    assert max(dark) <= 20, f"no window should be black, came out as {dark}"

print("Broadcast: file finalize + no overwrite, ON AIR on accept, escaped credentials, "
      "reconnect, refusal kept, stalled stop, every capture gap counted, "
      "RTMP picture paced in step and black without a window passed")
