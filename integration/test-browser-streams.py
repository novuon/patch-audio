#!/usr/bin/env python3
"""Native browser lanes on a private graph; never touches desktop audio.

Several differently buffered streams share one libpulse context/client, as
Chromium web apps can. Verify actual engine meter samples and independent DSP.
"""
import array
import ctypes as C
import ctypes.util
import json
import math
import os
from pathlib import Path
import queue
import signal
import socket
import subprocess
import sys
import tempfile
import threading
import time

STREAMS = 12
AMPLITUDE = .01


def wait(predicate, seconds=15):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        value = predicate()
        if value:
            return value
        time.sleep(.03)
    raise AssertionError("browser routing did not settle")


class Browser:
    """All streams belong to one Pulse context, driven on one thread."""
    def __init__(self, server):
        self.server = server
        self.commands = queue.Queue()
        self.errors = queue.Queue()
        self.stats = {"writes": 0, "bytes": 0, "states": {}}
        self.ready = threading.Event()
        self.thread = threading.Thread(target=self.run, daemon=True)
        self.thread.start()
        assert self.ready.wait(10), "libpulse fixture failed to start"
        self.check()

    def check(self):
        if not self.errors.empty():
            raise self.errors.get()

    def run(self):
        pa = C.CDLL(ctypes.util.find_library("pulse"))
        def api(name, result, *args):
            fn = getattr(pa, name)
            fn.restype, fn.argtypes = result, list(args)
            return fn
        ptr, integer, size, string = C.c_void_p, C.c_int, C.c_size_t, C.c_char_p
        class Spec(C.Structure):
            _fields_ = [("format", integer), ("rate", C.c_uint32), ("channels", C.c_uint8)]
        class Attr(C.Structure):
            _fields_ = [(name, C.c_uint32) for name in ("maxlength", "tlength", "prebuf", "minreq", "fragsize")]
        api("pa_mainloop_new", ptr)
        api("pa_mainloop_get_api", ptr, ptr)
        api("pa_mainloop_iterate", integer, ptr, integer, C.POINTER(integer))
        api("pa_mainloop_free", None, ptr)
        api("pa_proplist_new", ptr)
        api("pa_proplist_sets", integer, ptr, string, string)
        api("pa_proplist_free", None, ptr)
        api("pa_context_new_with_proplist", ptr, ptr, string, ptr)
        api("pa_context_connect", integer, ptr, string, integer, ptr)
        api("pa_context_get_state", integer, ptr)
        api("pa_context_disconnect", None, ptr)
        api("pa_context_unref", None, ptr)
        api("pa_stream_new", ptr, ptr, string, C.POINTER(Spec), ptr)
        class Volume(C.Structure):
            _fields_ = [("channels", C.c_uint8), ("values", C.c_uint32 * 32)]
        api("pa_stream_connect_playback", integer, ptr, string, C.POINTER(Attr), integer, ptr, ptr)
        api("pa_stream_get_state", integer, ptr)
        api("pa_stream_writable_size", size, ptr)
        api("pa_stream_write", integer, ptr, ptr, size, ptr, C.c_int64, integer)
        api("pa_stream_disconnect", integer, ptr)
        api("pa_stream_unref", None, ptr)
        loop = pa.pa_mainloop_new()
        properties = pa.pa_proplist_new()
        pa.pa_proplist_sets(properties, b"application.process.binary", b"chromium")
        context = pa.pa_context_new_with_proplist(pa.pa_mainloop_get_api(loop), b"Chromium", properties)
        pa.pa_proplist_free(properties)
        streams = {}
        try:
            # Explicit private server + NOAUTOSPAWN: cannot fall back to desktop.
            assert pa.pa_context_connect(context, self.server.encode(), 1, None) == 0
            deadline = time.monotonic() + 8
            while pa.pa_context_get_state(context) != 4:
                assert pa.pa_context_get_state(context) < 5 and time.monotonic() < deadline
                pa.pa_mainloop_iterate(loop, 0, None)
                time.sleep(.002)
            def add(index):
                spec = Spec(5, 48000, 2)  # PA_SAMPLE_FLOAT32LE
                stream = pa.pa_stream_new(context, (b"Playback" if index == 99 else f"Podcast {70 if index == 71 else index}".encode()), C.byref(spec), None)
                attr = Attr(0xffffffff, 8192 if index % 2 else 16384, 0xffffffff, 0xffffffff, 0xffffffff)
                volume = Volume(2, (C.c_uint32 * 32)(32768, 32768))
                assert pa.pa_stream_connect_playback(stream, b"patch", C.byref(attr), 0x2000 | (0x1000 if index == 1 else 0), C.byref(volume) if index == 0 else None, None) == 0
                amplitude = 0 if index == 99 else AMPLITUDE * (index + 1)
                samples = array.array("f", (amplitude * math.sin(2 * math.pi * 100 * (frame // 2) / 48000) for frame in range(480 * 2)))
                payload = C.create_string_buffer(samples.tobytes())
                streams[index] = [stream, payload, 0]
            for index in range(STREAMS):
                add(index)
            self.ready.set()
            running = True
            while running:
                for _ in range(4):
                    pa.pa_mainloop_iterate(loop, 0, None)
                while not self.commands.empty():
                    action, index = self.commands.get_nowait()
                    if action == "stop":
                        running = False
                        break
                    if action == "remove":
                        stream, _, _ = streams.pop(index)
                        pa.pa_stream_disconnect(stream)
                        pa.pa_stream_unref(stream)
                    if action == "add":
                        add(index)
                    if action in ("sound", "quiet"):
                        amplitude = .05 if action == "sound" else 0
                        samples = array.array("f", (amplitude * math.sin(2 * math.pi * 100 * (frame // 2) / 48000) for frame in range(480 * 2)))
                        streams[index][1] = C.create_string_buffer(samples.tobytes())
                for record in streams.values():
                    stream, payload, cursor = record
                    state = pa.pa_stream_get_state(stream)
                    self.stats["states"][str(stream)] = state
                    assert state < 3, f"stream failed: {state}"
                    if state != 2:
                        continue
                    count = pa.pa_stream_writable_size(stream)
                    assert count != C.c_size_t(-1).value
                    count = min(count, (480 - cursor) * 8) // 8 * 8
                    if count:
                        assert pa.pa_stream_write(stream, C.byref(payload, cursor * 8), count, None, 0, 0) == 0
                        self.stats["writes"] += 1
                        self.stats["bytes"] += count
                        record[2] = (cursor + count // 8) % 480
                time.sleep(.0002)
        except BaseException as error:
            self.errors.put(error)
            self.ready.set()
        finally:
            for stream, _, _ in streams.values():
                pa.pa_stream_disconnect(stream)
                pa.pa_stream_unref(stream)
            pa.pa_context_disconnect(context)
            pa.pa_context_unref(context)
            pa.pa_mainloop_free(loop)

    def close(self):
        self.commands.put(("stop", 0))
        self.thread.join(5)
        self.check()


if len(sys.argv) > 1 and sys.argv[1] == "--fixture":
    fixture = Browser(sys.argv[2])
    print("READY", flush=True)
    for line in sys.stdin:
        fixture.check()
        action, index = line.split()
        if action == "stop":
            break
        fixture.commands.put((action, int(index)))
    fixture.close()
    sys.exit(0)


class BrowserProcess:
    # Audio production has its own interpreter: parsing a large debug meter
    # reply must not starve this fixture through the parent's Python GIL.
    def __init__(self, server):
        self.process = subprocess.Popen([sys.executable, str(Path(__file__).resolve()), "--fixture", server],
                                        env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True)
        children.append(self.process)
        assert self.process.stdout.readline().strip() == "READY"
        self.commands = self
        self.stats = {"pid": self.process.pid}
    def put(self, command):
        action, index = command
        self.process.stdin.write(f"{action} {index}\n")
        self.process.stdin.flush()
    def check(self):
        assert self.process.poll() is None, "browser fixture process stopped"
    def close(self):
        if self.process.poll() is None:
            self.put(("stop", 0))
        assert self.process.wait(5) == 0


binary = Path(sys.argv[1]).resolve()
legacy = Path(sys.argv[sys.argv.index("--from-legacy") + 1]).resolve() if "--from-legacy" in sys.argv else None
donor = 10**(-6/20) if legacy else 1.0
with tempfile.TemporaryDirectory(prefix="patch-browser-streams-") as temp:
    root = Path(temp)
    runtime = root / "run"
    runtime.mkdir(mode=0o700)
    config = root / "config" / "omarchy-patch"
    config.mkdir(parents=True)
    (config / "setup.toml").write_text("system_audio = true\ndj = false\nbar = false\nthemes = false\n")
    policy = root / "config" / "wireplumber" / "wireplumber.conf.d"
    policy.mkdir(parents=True)
    (policy / "90-browser-test.conf").write_text('''
wireplumber.profiles = {
  browser-test = {
    inherits = [ base ]
    metadata.sm-settings = required
    metadata.sm-objects = required
    policy.standard = required
    hardware.audio = disabled
    hardware.bluetooth = disabled
    hardware.video-capture = disabled
    monitor.alsa = disabled
    monitor.alsa-midi = disabled
    monitor.bluez = disabled
    monitor.bluez-midi = disabled
    monitor.v4l2 = disabled
    monitor.libcamera = disabled
    support.dbus = disabled
    support.portal-permissionstore = disabled
    script.client.access-portal = disabled
  }
}
''')
    env = dict(os.environ, HOME=str(root / "home"), XDG_RUNTIME_DIR=str(runtime),
               XDG_CONFIG_HOME=str(root / "config"), XDG_STATE_HOME=str(root / "state"),
               XDG_CACHE_HOME=str(root / "cache"), XDG_DATA_HOME=str(root / "data"),
               PIPEWIRE_RUNTIME_DIR=str(runtime),
               PULSE_SERVER="unix:" + str(runtime / "pulse" / "native"),
               DBUS_SESSION_BUS_ADDRESS="unix:path=" + str(root / "no-bus"))
    for key in ("PIPEWIRE_REMOTE", "PIPEWIRE_CONFIG_DIR", "PULSE_RUNTIME_PATH"):
        env.pop(key, None)
    children, logs = [], []
    browser = None
    def start(args):
        log = open(root / f"process-{len(children)}.log", "wb")
        logs.append(log)
        p = subprocess.Popen(args, env=env, stdout=log, stderr=log)
        children.append(p)
        return p
    def pactl(*args):
        return subprocess.check_output(["pactl", *args], env=env, timeout=10, text=True)
    def request(expect_error=False, **value):
        with socket.socket(socket.AF_UNIX) as s:
            s.settimeout(10)
            s.connect(str(runtime / "omarchy-patch" / "engine.sock"))
            s.sendall((json.dumps(value) + "\n").encode())
            reply = json.loads(s.makefile().readline())
            if expect_error:
                assert "err" in reply, reply
                return reply["err"]
            assert "err" not in reply, reply
            return reply["ok"]
    def inputs():
        if browser:
            browser.check()
        return [s for s in json.loads(pactl("-f", "json", "list", "sink-inputs"))
                if s["properties"].get("application.name") == "Chromium"]
    def peak(channel):
        # Read blocks produced strictly after the control operation.
        since = request(op="meters")["seq"]
        time.sleep(.12)
        frame = request(op="meters", since=since)
        index = frame["channels"].index(channel)
        assert frame["blocks"], "engine supplied no meter samples"
        return max(max(b["ch"][index]) + 20*math.log10(max(max(b["gain"][index]), 1e-12)) for b in frame["blocks"][-3:])
    try:
        start(["pipewire"])
        wait(lambda: (runtime / "pipewire-0").exists())
        start(["pipewire-pulse"])
        wait(lambda: (runtime / "pulse" / "native").exists())
        start(["wireplumber", "--profile=browser-test"])
        wait(lambda: "default" in subprocess.check_output(["pw-metadata", "-l"], env=env, text=True))
        pactl("load-module", "module-null-sink", "sink_name=alsa_output.browser-test", "rate=48000", "channels=2")
        pactl("set-default-sink", "alsa_output.browser-test")
        start([str(legacy or binary), "engine"])
        wait(lambda: (runtime / "omarchy-patch" / "engine.sock").exists())
        wait(lambda: request(op="status")["linked"])
        wait(lambda: pactl("get-default-sink").strip() == "patch")
        before = time.monotonic()
        browser = BrowserProcess(env["PULSE_SERVER"])
        wait(lambda: len(inputs()) == STREAMS)
        if legacy:
            request(op="command", origin="user", command={"op": "eq.app", "app": "Chromium", "on": True})
            legacy_channel = request(op="eq_apps")["assigned"]["Chromium"]
            request(op="gain", channel=legacy_channel, db=-6)
            def legacy_routed():
                target = next((k["index"] for k in json.loads(pactl("-f", "json", "list", "sinks")) if k["name"] == "patch." + legacy_channel), None)
                return target is not None and all(s["sink"] == target for s in inputs())
            wait(legacy_routed)
            subprocess.run([str(binary), "engine", "upgrade"], env=env, check=True, timeout=30,
                           stdout=subprocess.PIPE, stderr=subprocess.PIPE)
            print("Upgraded legacy shared-app routing to native stream lanes")
        first = inputs()
        assert len({s["properties"]["client.id"] for s in first}) == 1, "fixture did not share a client"
        def routed():
            sources = inputs()
            sinks = {s["index"]: s["name"] for s in json.loads(pactl("-f", "json", "list", "sinks"))}
            names = [sinks.get(s["sink"], "") for s in sources]
            return len(names) == STREAMS and len(set(names)) == STREAMS and all(n.startswith("patch.stream-") for n in names)
        wait(routed)
        print(f"{STREAMS} shared-client streams independently routed in {(time.monotonic()-before)*1000:.1f} ms")
        assigned = request(op="eq_apps")["assigned"]
        assert len([k for k in assigned if k.startswith("stream:")]) == STREAMS
        sources = sorted(inputs(), key=lambda s: s["index"])
        lanes = [assigned[next(k for k in assigned if k.startswith(f'stream:{s["index"]}:'))] for s in sources]
        assert len(set(lanes)) == STREAMS
        wait(lambda: peak(lanes[0]) > -70)
        initial = {c["name"]: c for c in request(op="status")["channels"]}
        assert abs(initial[lanes[0]]["gain_db"] - 20*math.log10(.125 * donor)) < .01
        assert initial[lanes[1]]["mute"]
        assert peak(lanes[1]) < -100
        assert all(not s["mute"] and all(v["value"] == 65536 for v in s["volume"].values()) for s in inputs())
        request(op="mute", channel=lanes[1], muted=False)
        for i, lane in enumerate(lanes):
            expected = 20 * math.log10(AMPLITUDE * (i + 1) * (.125 if i == 0 else 1) * donor)
            measured = peak(lane)
            assert abs(measured - expected) < 1.0, (lane, measured, expected)
        original = peak(lanes[1])
        request(op="gain", channel=lanes[0], db=-12)
        assert abs(peak(lanes[0]) - (20*math.log10(AMPLITUDE)-12)) < 1
        assert abs(peak(lanes[1]) - original) < .2
        request(op="solo", channel=lanes[0], solo=True)
        assert peak(lanes[1]) < -100
        request(op="solo", channel=lanes[0], solo=False)
        assert abs(peak(lanes[1]) - original) < .2
        request(op="mute", channel=lanes[0], muted=True)
        assert peak(lanes[0]) < -100
        assert peak(lanes[1]) > -40
        request(op="mute", channel=lanes[0], muted=False)
        request(op="eq", channel=lanes[0], low_db=-12)
        assert peak(lanes[0]) < 20*math.log10(AMPLITUDE)-20
        assert abs(peak(lanes[1]) - original) < .2
        request(op="eq", channel=lanes[0], low_db=0)
        request(op="gain", channel=lanes[0], db=0)
        own_key = next(k for k, ch in assigned.items() if ch == lanes[0])
        request(op="master_eq", target="app:" + own_key, state={"bands": [
            {"on": True, "kind": "bell", "freq": 100, "gain_db": -9, "q": 1.0}]})
        assert abs(peak(lanes[0]) - (20*math.log10(AMPLITUDE)-9)) < 1
        assert abs(peak(lanes[1]) - original) < .2
        request(op="send", channel=lanes[0], bus=0, db=-6, pre=True)
        states = {s["name"]: s for s in request(op="status")["channels"]}
        assert states[lanes[0]]["sends"][0]["db"] == -6
        assert states[lanes[1]]["sends"][0]["db"] is None
        since = request(op="meters")["seq"]
        time.sleep(.15)
        sent = request(op="meters", since=since)["blocks"][-3:]
        assert abs(max(max(b["bus"][0]) for b in sent) - (20*math.log10(AMPLITUDE)-9-6)) < 1

        handover_state = {c["name"]: c for c in request(op="status")["channels"] if c["name"] in lanes}
        subprocess.run([str(binary), "engine", "upgrade"], env=env, check=True, timeout=30,
                       stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        wait(lambda: request(op="eq_apps")["assigned"] == assigned)
        for state in request(op="status")["channels"]:
            if state["name"] in handover_state:
                prior = handover_state[state["name"]]
                assert abs(state["gain_db"] - prior["gain_db"]) < .01
                assert state["mute"] == prior["mute"]
        assert abs(peak(lanes[0]) - (20*math.log10(AMPLITUDE)-9)) < 1
        assert abs(peak(lanes[1]) - original) < .2
        print("PASS: engine handover retains stream identities, levels and per-stream EQ")
        # A departing stream frees only its own lane. Every surviving route
        # stays attached to the same caller, regardless of list order.
        request(op="master_eq", target="app:" + own_key, state={"bands": []})
        old_keys = dict(assigned)
        browser.commands.put(("remove", 0))
        wait(lambda: len(inputs()) == STREAMS - 1)
        wait(lambda: len(request(op="eq_apps")["assigned"]) == STREAMS - 1)
        remaining = request(op="eq_apps")["assigned"]
        assert all(old_keys[k] == c for k, c in remaining.items())
        browser.commands.put(("add", STREAMS))
        wait(lambda: len(inputs()) == STREAMS)
        wait(routed)
        newest = max(inputs(), key=lambda s: s["index"])
        new_key = next(k for k in request(op="eq_apps")["assigned"] if k.startswith(f'stream:{newest["index"]}:'))
        reused = request(op="eq_apps")["assigned"][new_key]
        state = next(s for s in request(op="status")["channels"] if s["name"] == reused)
        # A browser reopens a paused tab as a new stream: the new stream
        # takes the application's last lane settings, never a mute or solo.
        profile = json.loads((root / "state" / "omarchy-patch" / "stream-profiles.json").read_text())["Chromium"]
        assert abs(state["gain_db"] - 20*math.log10(profile["gain"])) < .05 and not state["mute"] and not state["solo"]
        assert abs(state["eq"]["low_db"] - 20*math.log10(max(profile["eq"][0], 1e-6))) < .05
        sends = state["sends"][0]["db"]
        assert (sends is None) == (profile["sends"][0][0] == 0), (sends, profile["sends"][0])
        if not (profile.get("eq8") or {}).get("bands"):
            assert abs(peak(reused) - 20*math.log10(AMPLITUDE * (STREAMS + 1) * profile["gain"])) < 1
        old_key = next(k for k, ch in old_keys.items() if ch == lanes[0])
        error = request(op="gain", channel=reused, stream_key=old_key, db=-60, expect_error=True)
        assert "no longer owns" in error
        after = next(c for c in request(op="status")["channels"] if c["name"] == reused)
        assert abs(after["gain_db"] - state["gain_db"]) < .01
        request(op="gain", channel=reused, stream_key=new_key, db=-3)
        assert abs(peak(reused) - (20*math.log10(AMPLITUDE * (STREAMS + 1))-3)) < 1
        print("PASS: stale caller commands rejected after reuse; current owner controls accepted")

        print("PASS: real independent meters, gain, mute, EQ, sends, stable identity and clean lane reuse")
        if "--provider" in sys.argv:
            subprocess.run([str(binary.parent / "examples" / "contextual-check")], env=env, check=True, timeout=30)

        browser.commands.put(("add", 99))
        wait(lambda: len(inputs()) == STREAMS + 1)
        # The graph may already have discovered it; identify by its exposed media name.
        helper = next(s for s in inputs() if s["properties"].get("media.name") == "Playback")
        key = wait(lambda: next((k for k in request(op="eq_apps")["assigned"] if k.startswith(f'stream:{helper["index"]}:')), None))
        wait(lambda: key in request(op="eq_apps")["hidden"])
        time.sleep(.5)
        assert key in request(op="eq_apps")["hidden"]
        if "--provider" in sys.argv:
            subprocess.run([str(binary.parent / "examples" / "contextual-check")], env=dict(env, PATCH_TEST_HIDDEN_STREAM=key), check=True, timeout=30)
        # A click as the helper opens is not a caller: a second of signal is.
        browser.commands.put(("sound", 99))
        time.sleep(.2)
        browser.commands.put(("quiet", 99))
        time.sleep(.5)
        assert key in request(op="eq_apps")["hidden"], "a short click must not reveal a silent helper"
        browser.commands.put(("sound", 99))
        wait(lambda: key not in request(op="eq_apps")["hidden"])
        browser.commands.put(("quiet", 99))
        time.sleep(.5)
        assert key not in request(op="eq_apps")["hidden"], "quiet callers must retain their strip"
        subprocess.run([str(binary), "engine", "upgrade"], env=env, check=True, timeout=30,
                       stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        time.sleep(.5)
        assert key not in request(op="eq_apps")["hidden"], "quiet caller visibility must survive handover"
        print("PASS: silent browser helper hidden, a click does not reveal it, a second of audio does, quiet caller retained across silence and handover")

        # A browser moves its sound to a new stream of the same process and
        # keeps the old one open, quiet (Chromium reopens its playback when a
        # page starts an echo-cancelling capture). The new stream takes the
        # old lane: its strip, the user's fader and a Remote voice follow.
        talk_lane = lanes[2]
        for role, input_send in (("voice", True), ("far_end", False)):
            picked = request(op="mic_remote", channel="mic-5", role=role)
            status = request(op="status")
            mic = next(m for m in status["mics"] if m["channel"] == "mic-5")
            assert mic["input_send"] is input_send, mic
            assert status["remote_lanes"][picked["lane"]] == "mic-5", status["remote_lanes"]
            request(op="mic_send", channel="mic-5", on=not input_send)
            mic = next(m for m in request(op="status")["mics"] if m["channel"] == "mic-5")
            assert mic["input_send"] is not input_send, mic
            request(op="mic", channel="mic-5", source=None)
        print("PASS: browser lane role controls capture and sends and can be reversed")

        talk_key = next(k for k, ch in request(op="eq_apps")["assigned"].items() if ch == talk_lane)
        request(op="gain", channel=talk_lane, stream_key=talk_key, db=-4)
        request(op="mic", channel="mic-5", source=talk_key)
        browser.commands.put(("add", 70))
        browser.commands.put(("quiet", 70))
        wait(lambda: len(inputs()) == STREAMS + 2)
        newest = max(inputs(), key=lambda s: s["index"])
        new_key = wait(lambda: next((k for k in request(op="eq_apps")["assigned"] if k.startswith(f'stream:{newest["index"]}:')), None))
        assert request(op="eq_apps")["assigned"][new_key] != talk_lane
        time.sleep(3.5)  # the helper above is long quiet: no handover candidate
        browser.commands.put(("quiet", 2))
        browser.commands.put(("sound", 70))
        wait(lambda: request(op="eq_apps")["assigned"].get(new_key) == talk_lane, seconds=8)
        assert request(op="eq_apps")["assigned"][talk_key] != talk_lane
        state = next(c for c in request(op="status")["channels"] if c["name"] == talk_lane)
        assert abs(state["gain_db"] + 4) < .05, state["gain_db"]
        wait(lambda: peak("mic-5") > -60)
        remote = next(m for m in request(op="mics")["mics"] if m["channel"] == "mic-5")
        assert remote["source"] == new_key and remote["present"], remote
        print("PASS: a stream that takes over a quiet sibling's sound keeps its lane, fader and Remote voice")
        # Two sources after all (both play): the lanes go back.
        browser.commands.put(("sound", 2))
        wait(lambda: request(op="eq_apps")["assigned"].get(talk_key) == talk_lane, seconds=10)
        print("PASS: two playing streams are no handover: the swap is undone")
        request(op="mic", channel="mic-5", source=None)
        # Closed and reopened: the same lane comes back.
        before = request(op="eq_apps")["assigned"][new_key]
        request(op="gain", channel=before, stream_key=new_key, db=-11)
        browser.commands.put(("remove", 70))
        wait(lambda: new_key not in request(op="eq_apps")["assigned"])
        browser.commands.put(("add", 71))
        wait(lambda: len(inputs()) == STREAMS + 2)
        newest = max(inputs(), key=lambda stream: stream["index"])
        reopened = wait(lambda: next((k for k in request(op="eq_apps")["assigned"] if k.startswith(f'stream:{newest["index"]}:')), None))
        assert request(op="eq_apps")["assigned"][reopened] == before
        state = next(c for c in request(op="status")["channels"] if c["name"] == before)
        assert abs(state["gain_db"] + 11) < .05, state["gain_db"]
        print("PASS: a reopened stream takes back its lane and retains its fader")

    finally:
        if browser:
            browser.close()
        # A handover successor is detached from the original child process.
        # Stop it through this private socket before removing its servers.
        successor = None
        try:
            successor = request(op="status")["pid"]
            request(op="quit", handover=True)
            wait(lambda: not Path(f"/proc/{successor}/exe").exists(), seconds=5)
        except (OSError, AssertionError):
            if successor:
                try:
                    os.kill(successor, signal.SIGTERM)
                except ProcessLookupError:
                    pass
        for p in reversed(children):
            if p.poll() is None:
                p.terminate()
                try:
                    p.wait(3)
                except subprocess.TimeoutExpired:
                    p.kill()
                    p.wait()
        for log in logs:
            log.close()
        if sys.exc_info()[0]:
            for log in root.glob("process-*.log"):
                print(log.name, log.read_text(errors="replace")[-3000:], file=sys.stderr)
