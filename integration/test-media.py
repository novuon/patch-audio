#!/usr/bin/env python3
"""Isolated recorder/player regression test; never opens real audio outputs.

Run after cargo build --offline --release -p patch:
    python3 integration/test-media.py target/release/omarchy-patch
Uses a fake capture pipe and mpv's null output in a private runtime directory.
"""
import functools
import http.server
import json
import resource
import signal as signals
import threading
import os
from pathlib import Path
import shutil
import socket
import struct
import subprocess
import sys
import tempfile
import time
import wave

binary = Path(sys.argv[1] if len(sys.argv) > 1 else "target/release/omarchy-patch").resolve()
mpv = shutil.which("mpv")
assert mpv, "mpv is required for the isolated playback test"


def wait_for(predicate, timeout=5):
    end = time.monotonic() + timeout
    while time.monotonic() < end:
        value = predicate()
        if value:
            return value
        time.sleep(0.02)
    raise AssertionError("timed out")


def socket_call(path, request):
    with socket.socket(socket.AF_UNIX) as stream:
        stream.settimeout(1)
        stream.connect(str(path))
        stream.sendall((request + "\n").encode())
        return stream.makefile().readline()


with tempfile.TemporaryDirectory(prefix="patch-media-test-") as temp:
    root = Path(temp)
    helpers = root / "bin"
    helpers.mkdir()
    runtime = root / "runtime"
    runtime.mkdir(mode=0o700)
    env = dict(
        os.environ,
        XDG_RUNTIME_DIR=str(runtime),
        XDG_CONFIG_HOME=str(root / "config"),
        XDG_STATE_HOME=str(root / "state"),
        XDG_CACHE_HOME=str(root / "cache"),
        XDG_DATA_HOME=str(root / "data"),
        PATH=str(helpers) + os.pathsep + os.environ["PATH"],
    )
    capture = helpers / "parec"
    capture.write_text("""#!/usr/bin/env python3
import os, struct, sys, time
from pathlib import Path
if os.environ.get('REC_FAIL'):
    print('simulated unavailable capture source', file=sys.stderr)
    raise SystemExit(17)
elif os.environ.get('REC_PCM'):
    data = Path(os.environ['REC_PCM']).read_bytes()
    for i in range(0, len(data), 8192):
        os.write(1, data[i:i+8192])
    Path(os.environ['REC_READY']).touch()
    time.sleep(60)
else:
    while True:
        os.write(1, struct.pack('<ff', .25, -.25) * 480)
        time.sleep(.01)
""")
    capture.chmod(0o755)
    wrapper = helpers / "mpv"
    wrapper.write_text("#!/usr/bin/env python3\nimport os, sys\nmpv = " + repr(mpv) + "\nargs = ['--ao=null' if a == '--ao=pipewire' else a for a in sys.argv[1:]]\nos.execv(mpv, [mpv] + args)\n")
    wrapper.chmod(0o755)
    sock = runtime / "omarchy-patch" / "recorder.sock"

    def cli(*args):
        return subprocess.run([str(binary), *args], env=env, check=True, capture_output=True, text=True, timeout=10).stdout.strip()

    def launch_take(path, extra=None):
        process = subprocess.Popen([str(binary), "record-daemon", "device:test", "f32", "48000", str(path), "Regression capture"], env=dict(env, **(extra or {})), stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        wait_for(sock.exists)
        wait_for(lambda: json.loads(socket_call(sock, "status"))["frames"] > 0)
        return process

    # Stop must drain all complete frames already captured, then finalize sizes.
    pcm = root / "signal.raw"
    signal = b"".join(struct.pack("<ff", (i % 101) / 101.0, -(i % 97) / 97.0) for i in range(65536))
    pcm.write_bytes(signal)
    ready = root / "ready"
    take = root / "tail.wav"
    process = launch_take(take, {"REC_PCM": str(pcm), "REC_READY": str(ready)})
    wait_for(ready.exists)
    assert cli("record", "stop") == "stopped"
    out, err = process.communicate(timeout=10)
    assert process.returncode == 0, err.decode()
    wav = take.read_bytes()
    data = wav.index(b"data")
    size = struct.unpack_from("<I", wav, data + 4)[0]
    assert size == len(signal), (size, len(signal))
    assert wav[data + 8:data + 8 + size] == signal
    assert struct.unpack_from("<I", wav, 4)[0] == len(wav) - 8
    result = json.loads(sock.with_suffix(".result.json").read_text())
    assert result["frames"] == 65536 and result["error"] is None
    assert not sock.exists()
    print("PASS capture drain, float identity, final header and outcome")

    # An existing take must survive a failed attempt to reuse its name.
    result = subprocess.run([str(binary), "record-daemon", "device:test", "f32", "48000", str(take), "Duplicate"], env=env, capture_output=True)
    assert result.returncode != 0 and take.read_bytes() == wav and not sock.exists()
    print("PASS overwrite protection and failed-start socket cleanup")

    process = launch_take(root / "pause.wav")
    try:
        # A client that never sends a line cannot stall the recorder indefinitely.
        with socket.socket(socket.AF_UNIX) as stalled:
            stalled.connect(str(sock))
            start = time.monotonic()
            assert json.loads(socket_call(sock, "status"))["recording"]
            assert time.monotonic() - start < .8
        # A second recorder cannot unlink or replace the first one's socket.
        duplicate = subprocess.run([str(binary), "record-daemon", "device:test", "f32", "48000", str(root / "duplicate.wav"), "Duplicate"], env=env, capture_output=True)
        assert duplicate.returncode != 0 and not (root / "duplicate.wav").exists()
        socket_call(sock, "pause")
        time.sleep(.12)
        before = json.loads(socket_call(sock, "status"))
        time.sleep(.20)
        after = json.loads(socket_call(sock, "status"))
        assert after["paused"] and after["frames"] == before["frames"]
        assert after["dropouts"] == before["dropouts"]
        socket_call(sock, "resume")
        wait_for(lambda: json.loads(socket_call(sock, "status"))["frames"] > after["frames"])
        socket_call(sock, "stop")
        _, err = process.communicate(timeout=10)
        assert process.returncode == 0, err.decode()
        print("PASS exclusive recorder, pause/resume, no false pause dropouts")
    finally:
        if process.poll() is None:
            process.kill()
            process.wait()

    # A capture source that exits during startup must leave a visible reason.
    failed_take = root / "capture-start-error.wav"
    failed = subprocess.Popen([str(binary), "record-daemon", "device:test", "f32", "48000", str(failed_take), "Capture startup error"], env=dict(env, REC_FAIL="1"), stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    outcome = sock.with_suffix(".result.json")
    wait_for(lambda: outcome.exists() and json.loads(outcome.read_text()).get("path") == str(failed_take))
    _, err = failed.communicate(timeout=10)
    result = json.loads(outcome.read_text())
    assert failed.returncode != 0, err.decode()
    assert "simulated unavailable capture source" in result["error"], result
    print("PASS unexpected capture exit preserves its source error")

    # A filesystem write failure must produce a retained error, not "saved".
    def small_file_limit():
        signals.signal(signals.SIGXFSZ, signals.SIG_IGN)
        resource.setrlimit(resource.RLIMIT_FSIZE, (1024, 1024))
    failed = subprocess.run([str(binary), "record-daemon", "device:test", "f32", "48000", str(root / "disk-error.wav"), "Disk error"], env=env, preexec_fn=small_file_limit, capture_output=True, timeout=10)
    assert failed.returncode != 0
    result = json.loads(sock.with_suffix(".result.json").read_text())
    assert result["error"] and not sock.exists()
    print("PASS write failure retained and socket timeout bounded")

    # Real decoding/transport with a silent null device; no DJ/engine/socket sharing.
    audio = root / 'Audio with "quotes".wav'
    with wave.open(str(audio), "wb") as output:
        output.setnchannels(2)
        output.setsampwidth(2)
        output.setframerate(48000)
        output.writeframes(b"\0\0\0\0" * 48000 * 8)
    try:
        cli("player", "load", str(audio))
        def status():
            return json.loads(cli("player", "status"))
        wait_for(lambda: status()["playing"] and status()["duration"] > 7.9)
        cli("player", "pause")
        assert not status()["playing"]
        cli("player", "seek", "50%")
        wait_for(lambda: abs(status()["position"] - 4) < .3)
        cli("player", "volume", "37")
        assert status()["volume"] == 37
        # Separate CLI processes share cue state in the detached mpv, in seconds.
        cli("player", "cue")
        saved_cue = status()["cue_point"]
        assert abs(saved_cue - 4) < .3
        cli("player", "seek", "75%")
        wait_for(lambda: abs(status()["position"] - 6) < .3)
        cli("player", "cue")
        wait_for(lambda: abs(status()["position"] - saved_cue) < .3)
        assert not status()["playing"] and status()["volume"] == 37
        cli("player", "cue")
        wait_for(lambda: status()["position"] < .15)
        assert status()["cue_point"] == 0
        cli("player", "seek", "50%")
        wait_for(lambda: abs(status()["position"] - 4) < .3)
        cli("player", "cue")
        cli("player", "load", str(audio))
        wait_for(lambda: status()["playing"] and status()["position"] < 1)
        cli("player", "pause")
        assert status()["cue_point"] == 0, "loading even the same file clears the old cue"

        # Engine requests stay inside this fixture. Exercise real CLI/mpv volume
        # while a small authoritative engine double supplies the cue route.
        engine_socket = sock.parent / "engine.sock"
        engine_state = {"cue_output": None, "assigned": False, "pfl": False}
        engine_errors = []
        engine_stop = threading.Event()
        engine_listener = socket.socket(socket.AF_UNIX)
        engine_listener.bind(str(engine_socket))
        engine_listener.listen()
        engine_listener.settimeout(.1)

        def cue_engine():
            while not engine_stop.is_set():
                try:
                    connection, _ = engine_listener.accept()
                except socket.timeout:
                    continue
                with connection, connection.makefile() as reader:
                    for line in reader:
                        request = json.loads(line)
                        try:
                            if request["op"] == "eq_apps":
                                reply = {"assigned": {"Patch Player": "app-2"} if engine_state["assigned"] else {}}
                            elif request["op"] == "status":
                                reply = {"cue_output": engine_state["cue_output"], "channels": [{"name": "app-2", "pfl": engine_state["pfl"]}]}
                            elif request["op"] == "command":
                                assert request["command"] == {"op": "eq.app", "app": "Patch Player", "on": True}
                                engine_state["assigned"] = True
                                reply = "ok"
                            elif request["op"] == "pfl":
                                assert request["channel"] == "app-2"
                                engine_state["pfl"] = request["on"]
                                reply = "ok"
                            else:
                                raise AssertionError(request)
                            response = {"ok": reply}
                        except Exception as error:
                            engine_errors.append(str(error))
                            response = {"err": str(error)}
                        connection.sendall((json.dumps(response) + "\n").encode())

        engine_thread = threading.Thread(target=cue_engine, daemon=True)
        engine_thread.start()
        try:
            rejected = subprocess.run([str(binary), "player", "pfl", "on"], env=env, capture_output=True, text=True, timeout=10)
            assert rejected.returncode != 0 and "Headphone Cue" in rejected.stderr
            assert not engine_state["assigned"] and status()["volume"] == 37
            engine_state["cue_output"] = "headphones"
            cli("player", "pfl", "on")
            assert status()["pfl"] and status()["volume"] == 37
            cli("player", "pfl", "off")
            assert not status()["pfl"] and status()["volume"] == 37
            engine_state["pfl"] = True  # A Mixer change is visible to a fresh CLI.
            assert status()["pfl"]
            assert not engine_errors, engine_errors
        finally:
            engine_stop.set()
            engine_thread.join(timeout=2)
            engine_listener.close()
            engine_socket.unlink()
        print("PASS cue recall across CLI processes and engine PFL without changing volume")
        cli("player", "loop", "on")
        assert status()["looping"]
        cli("player", "play")
        assert status()["playing"]
        cli("player", "loop", "off")
        cli("player", "seek", "100%")
        wait_for(lambda: not status()["playing"])
        cli("player", "play")
        wait_for(lambda: status()["playing"] and status()["position"] < 1)
        cli("player", "stop")
        wait_for(lambda: status()["position"] < .15)
        assert not status()["playing"]
        class QuietHandler(http.server.SimpleHTTPRequestHandler):
            def log_message(self, *args):
                pass
        server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), functools.partial(QuietHandler, directory=str(root)))
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        try:
            from urllib.parse import quote
            url = f"http://127.0.0.1:{server.server_port}/{quote(audio.name)}"
            cli("player", "load", url)
            wait_for(lambda: status()["playing"] and status()["path"] == url and status()["duration"] > 7.9)
            cli("player", "pause")
        finally:
            server.shutdown()
            server.server_close()
        print("PASS EOF restart and HTTP audio playback")

        # The carts: pads that play over the transport, each on a voice of
        # its own, driven by separate CLI processes sharing one board.
        short = root / "sting.wav"
        with wave.open(str(short), "wb") as output:
            output.setnchannels(2)
            output.setsampwidth(2)
            output.setframerate(48000)
            output.writeframes(b"\0\0\0\0" * 24000)
        def carts():
            return json.loads(cli("player", "cart", "status"))
        cli("player", "cart", "set", "1", str(audio), "Bed")
        cli("player", "cart", "set", "2", str(short))
        board = carts()["board"]
        pads = board["banks"][0]["pads"]
        assert pads[0]["label"] == "Bed" and abs(pads[0]["seconds"] - 8) < .1 and pads[1]["file"] == str(short)
        assert len(board["banks"]) == 4 and all(len(b["pads"]) == 12 for b in board["banks"])
        empty = subprocess.run([str(binary), "player", "cart", "9"], env=env, capture_output=True, text=True, timeout=10)
        assert empty.returncode != 0 and "no sound" in empty.stderr
        start = time.monotonic()
        cli("player", "cart", "1")
        first_press = (time.monotonic() - start) * 1000
        wait_for(lambda: [p["label"] for p in carts()["playing"]] == ["Bed"])
        # A spare voice waits now: the next pad starts on it, over the bed.
        start = time.monotonic()
        cli("player", "cart", "2")
        warm_press = (time.monotonic() - start) * 1000
        wait_for(lambda: sorted(p["pad"] for p in carts()["playing"]) == [0, 1])
        # The sting ends by itself and its voice with it; the bed plays on.
        wait_for(lambda: [p["pad"] for p in carts()["playing"]] == [0])
        assert carts()["playing"][0]["position"] > 0
        # The same pad again stops it.
        cli("player", "cart", "1")
        wait_for(lambda: carts()["playing"] == [])
        # Another bank by name; a fade ends the sound, and the CLI waits for it.
        cli("player", "cart", "bank", "commercials")
        cli("player", "cart", "set", "=", str(audio))
        assert carts()["board"]["bank"] == 1
        cli("player", "cart", "commercials", "12")
        wait_for(lambda: [(p["bank"], p["pad"]) for p in carts()["playing"]] == [(1, 11)])
        start = time.monotonic()
        cli("player", "cart", "fade")
        assert time.monotonic() - start > 1.5 and carts()["playing"] == []
        cli("player", "cart", "intros", "1")
        cli("player", "cart", "stop")
        wait_for(lambda: carts()["playing"] == [])
        cli("player", "cart", "clear", "12")
        assert not carts()["board"]["banks"][1]["pads"][11]["file"]
        # A pad set to duck turns the transport down under it and brings it
        # back when it ends: its own voice asks, no window needed.
        cli("player", "cart", "bank", "1")
        cli("player", "cart", "option", "1", "duck", "on")
        cli("player", "cart", "colour", "1", "4")
        pad = carts()["board"]["banks"][0]["pads"][0]
        assert pad["duck"] and pad["colour"] == 4 and not pad["hold"]
        assert status()["gain_db"] == 0 and status()["volume"] == 37
        cli("player", "cart", "1")
        wait_for(lambda: abs(status()["gain_db"] + 12) < 0.2)
        assert status()["volume"] == 37, "ducking never touches the volume the user set"
        # One ducking voice ending must not lift another one's duck.
        cli("player", "cart", "option", "2", "duck", "on")
        cli("player", "cart", "2")
        wait_for(lambda: sorted(p["pad"] for p in carts()["playing"]) == [0, 1])
        wait_for(lambda: [p["pad"] for p in carts()["playing"]] == [0])
        assert abs(status()["gain_db"] + 12) < 0.2
        cli("player", "cart", "stop")
        wait_for(lambda: abs(status()["gain_db"]) < 0.2)
        # A lone short voice restores the level on natural EOF as well.
        cli("player", "cart", "2")
        wait_for(lambda: status()["gain_db"] < -6)
        wait_for(lambda: not carts()["playing"] and abs(status()["gain_db"]) < 0.2)
        assert status()["volume"] == 37
        cli("player", "cart", "option", "1", "duck", "off")
        cli("player", "cart", "option", "2", "duck", "off")
        print(f"PASS carts over the transport: first press {first_press:.0f} ms, with the spare voice {warm_press:.0f} ms (CLI start to return)")

        # The queue: sources in order, kept by the player process itself, so
        # separate CLI processes (and windows) see one list.
        second = root / "second.wav"
        with wave.open(str(second), "wb") as output:
            output.setnchannels(2)
            output.setsampwidth(2)
            output.setframerate(48000)
            output.writeframes(b"\0\0\0\0" * 48000 * 6)
        cli("player", "load", str(audio))
        wait_for(lambda: status()["playing"] and status()["path"] == str(audio))
        cli("player", "pause")
        cli("player", "add", str(second), str(short))
        assert status()["queue"] == [str(audio), str(second), str(short)] and status()["queue_at"] == 0
        cli("player", "queue", "2")
        wait_for(lambda: status()["queue_at"] == 1 and status()["playing"] and status()["path"] == str(second))
        cli("player", "speed", "1.5")
        assert status()["speed"] == 1.5
        cli("player", "prev")  # within its first seconds: the entry before
        wait_for(lambda: status()["queue_at"] == 0)
        cli("player", "next")
        wait_for(lambda: status()["queue_at"] == 1)
        cli("player", "repeat", "all")
        assert status()["repeat_all"] and not status()["looping"]
        cli("player", "repeat", "one")
        assert status()["looping"] and not status()["repeat_all"]
        cli("player", "repeat", "off")
        assert not status()["looping"] and not status()["repeat_all"]
        cli("player", "pause")
        cli("player", "ab")
        assert status()["ab_a"] >= 0 and status()["ab_b"] < 0
        cli("player", "ab")
        assert status()["ab_b"] >= 0
        cli("player", "ab")
        assert status()["ab_a"] < 0 and status()["ab_b"] < 0
        # Opening a file while a queue stands plays it now and keeps the queue.
        cli("player", "load", str(short))
        wait_for(lambda: status()["path"] == str(short))
        assert status()["queue"] == [str(audio), str(second), str(short), str(short)] and status()["queue_at"] == 2
        cli("player", "pause")
        cli("player", "queue", "clear")
        assert status()["queue"] == [str(short)]
        cli("player", "speed", "1")
        print("PASS queue, speed, repeat and A-B loop across CLI processes")

        # A long file goes on where it was left; a short one starts over.
        long = root / "episode.wav"
        with wave.open(str(long), "wb") as output:
            output.setnchannels(1)
            output.setsampwidth(1)
            output.setframerate(8000)
            output.writeframes(b"\x80" * 8000 * 660)
        cli("player", "load", str(long))
        wait_for(lambda: status()["playing"] and status()["duration"] > 659)
        cli("player", "pause")
        cli("player", "seek", "50%")
        wait_for(lambda: abs(status()["position"] - 330) < 1)
        cli("player", "play")
        cli("player", "pause")  # a pause keeps the place
        cli("player", "load", str(audio))
        wait_for(lambda: status()["path"] == str(audio) and status()["playing"] and status()["position"] < 2)
        cli("player", "load", str(long))
        wait_for(lambda: status()["path"] == str(long) and status()["playing"] and 320 < status()["position"] < 335)
        cli("player", "stop")  # Stop: from the start next time
        cli("player", "load", str(audio))
        wait_for(lambda: status()["path"] == str(audio) and status()["playing"])
        cli("player", "load", str(long))
        wait_for(lambda: status()["path"] == str(long) and status()["playing"] and status()["position"] < 5)
        cli("player", "pause")
        print("PASS a long file resumes where it was left, and Stop starts it over")

        # Marks: named places kept per file, listed as timestamps.
        # Seek inside the second: mpv may land slightly before the target.
        cli("player", "goto", "120.5")
        wait_for(lambda: 120 <= status()["position"] < 121)
        marked = cli("player", "mark", "Sponsor", "read")
        assert marked == "02:00", (marked, status()["position"])
        cli("player", "goto", "30.5")
        wait_for(lambda: 30 <= status()["position"] < 31)
        assert cli("player", "mark") == "00:30"
        assert cli("player", "mark") == "00:30", "the same place is the same mark"
        assert cli("player", "marks") == "00:30\n02:00 Sponsor read"
        print("PASS marks are kept per file and list as timestamps")
        timings = []
        for _ in range(20):
            start = time.monotonic()
            status()
            timings.append((time.monotonic() - start) * 1000)
        timings.sort()
        print(f"PASS native mpv transport; status CLI median {timings[len(timings)//2]:.1f} ms, max {timings[-1]:.1f} ms")
    finally:
        cli("player", "quit")
