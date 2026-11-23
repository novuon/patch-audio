#!/usr/bin/env python3
"""Exercise routing on private PipeWire/Pulse servers with null destinations.

Never connects to the desktop graph or opens a physical audio device. Pass
--baseline to measure the old bar helper's bypass on an older release.
"""
import array
import json
import math
import os
from pathlib import Path
import socket
import struct
import subprocess
import sys
import tempfile
import threading
import time
import wave


binary = Path(sys.argv[1]).resolve()
baseline = "--baseline" in sys.argv[2:]


def wait(predicate, seconds=10):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        result = predicate()
        if result:
            return result
        time.sleep(0.02)
    raise AssertionError("routing condition did not settle")


with tempfile.TemporaryDirectory(prefix="patch-core-routing-") as temporary:
    root = Path(temporary)
    runtime = root / "run"
    runtime.mkdir(mode=0o700)
    config = root / "config" / "omarchy-patch"
    config.mkdir(parents=True)
    (config / "setup.toml").write_text(
        "system_audio = true\ndj = false\nstart_with_session = false\nbar = false\nthemes = false\n"
    )
    policy = root / "config" / "wireplumber" / "wireplumber.conf.d"
    policy.mkdir(parents=True)
    (policy / "90-routing-test.conf").write_text("""
wireplumber.profiles = {
  routing-test = {
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
""")
    env = dict(os.environ, HOME=str(root / "home"), XDG_RUNTIME_DIR=str(runtime),
               XDG_CONFIG_HOME=str(root / "config"), XDG_STATE_HOME=str(root / "state"),
               XDG_CACHE_HOME=str(root / "cache"), XDG_DATA_HOME=str(root / "data"),
               PIPEWIRE_RUNTIME_DIR=str(runtime),
               PULSE_SERVER="unix:" + str(runtime / "pulse" / "native"),
               DBUS_SESSION_BUS_ADDRESS="unix:path=" + str(root / "no-session-bus"))
    for key in ("PIPEWIRE_REMOTE", "PIPEWIRE_CONFIG_DIR", "PULSE_RUNTIME_PATH"):
        env.pop(key, None)
    children = []
    logs = []

    def start(arguments):
        log = open(root / f"process-{len(children)}.log", "wb")
        logs.append(log)
        process = subprocess.Popen(arguments, env=env, stdout=log, stderr=log)
        children.append(process)
        return process

    def run(arguments):
        # The contextual check assigns physical ports. That reply waits out a
        # link pass (AUX virtual sources), which is longer than a pactl query.
        timeout = 45 if any(str(arg).endswith("contextual-check") for arg in arguments) else 10
        result = subprocess.run(arguments, env=env, timeout=timeout, stdout=subprocess.PIPE,
                                stderr=subprocess.STDOUT, check=False)
        assert result.returncode == 0, (arguments, result.stdout.decode(errors="replace"))
        return result.stdout.decode().strip()

    def pactl(*arguments):
        return run(["pactl", *arguments])

    def request(**value):
        with socket.socket(socket.AF_UNIX) as connection:
            connection.settimeout(5)
            connection.connect(str(runtime / "omarchy-patch" / "engine.sock"))
            connection.sendall((json.dumps(value) + "\n").encode())
            reply = json.loads(connection.makefile().readline())
            assert "err" not in reply, reply
            return reply["ok"]

    try:
        start(["pipewire"])
        wait(lambda: (runtime / "pipewire-0").exists())
        start(["pipewire-pulse"])
        wait(lambda: (runtime / "pulse" / "native").exists())
        start(["wireplumber", "--profile=routing-test"])
        wait(lambda: "default" in run(["pw-metadata", "-l"]))
        speakers = "alsa_output.routing-test-speakers"
        phones = "alsa_output.routing-test-headphones"
        for destination in (speakers, phones):
            pactl("load-module", "module-null-sink", f"sink_name={destination}",
                  "channels=2", "channel_map=front-left,front-right", "rate=48000")
        wait(lambda: all(name in pactl("list", "short", "sinks") for name in (speakers, phones)))
        pactl("set-default-sink", speakers)
        pactl("set-sink-volume", speakers, "15%")
        if not baseline:
            # Old pipewire-pulse modules survive an engine upgrade. Reproduce
            # their truncated labels and missing nested passive property.
            for name, description, source in (("patch", "Patch", False),
                                              ("patch.system-sounds", "Patch System Sounds", False),
                                              ("patch.input", "Patch Input", True)):
                pactl("load-module", "module-null-sink", f"sink_name={name}",
                      f'sink_properties=device.description="{description}" node.passive=true',
                      "media.class=Audio/Source/Virtual" if source else "media.class=Audio/Sink",
                      "format=float32le", "rate=48000", "channels=2", "channel_map=front-left,front-right")
        start([str(binary), "engine"])
        wait(lambda: (runtime / "omarchy-patch" / "engine.sock").exists())
        wait(lambda: request(op="status").get("linked"))
        wait(lambda: request(op="status").get("system_post"))
        wait(lambda: pactl("get-default-sink") == "patch")
        def device_names_repaired():
            sinks = json.loads(pactl("-f", "json", "list", "sinks"))
            sources = json.loads(pactl("-f", "json", "list", "sources"))
            expected = {"patch": "Patch Output", "patch.system-sounds": "Patch System Sounds",
                        "patch.input": "Patch Input"}
            return all(sum(endpoint["name"] == name for endpoint in sinks + sources) == 1
                       and next(endpoint for endpoint in sinks + sources if endpoint["name"] == name)["description"] == label
                       for name, label in expected.items())
        if not baseline:
            wait(device_names_repaired)
        pactl("set-sink-volume", "patch", "15%")
        wait(lambda: 0 < request(op="status")["system_volume"]["left"] < 0.004)

        tone = root / "tone.wav"
        with wave.open(str(tone), "wb") as wav:
            wav.setparams((2, 2, 48000, 0, "NONE", "not compressed"))
            wav.writeframes(b"".join(struct.pack("<hh", value, value)
                                     for frame in range(48000)
                                     for value in [round(3276 * math.sin(2 * math.pi * 1000 * frame / 48000))]))
        # Match Patch Player's native PipeWire backend. The private policy
        # manager has every hardware monitor explicitly disabled.
        start(["mpv", "--no-config", "--no-video", "--really-quiet", "--ao=pipewire",
               *([] if baseline else ["--audio-device=pipewire/patch"]),
               "--audio-client-name=Patch Player", "--volume=100", "--loop-file=inf", str(tone)])

        def player():
            return next((stream for stream in json.loads(pactl("-f", "json", "list", "sink-inputs"))
                         if stream["properties"].get("application.name") == "Patch Player"), None)

        wait(player)
        captures = {}
        capture_lock = threading.Lock()
        for destination in (speakers, phones):
            process = subprocess.Popen(["parec", "--raw", "--format=float32le", "--rate=48000",
                                        "--channels=2", f"--device={destination}.monitor"],
                                       env=env, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)
            children.append(process)
            captures[destination] = []

            def collect(process=process, destination=destination):
                while data := process.stdout.read(4096):
                    samples = array.array("f")
                    samples.frombytes(data)
                    with capture_lock:
                        captures[destination].append((time.monotonic(), max(map(abs, samples), default=0)))

            threading.Thread(target=collect, daemon=True).start()
        wait(lambda: captures[speakers] and max(p for _, p in captures[speakers]) > 1e-5)
        time.sleep(0.4)
        gain = request(op="status")["system_volume"]
        expected_peak = 0.1 * gain["left"]
        marker = time.monotonic()
        timings = []
        if baseline:
            # Use the actual stock helper, including wpctl's preferred default.
            # Repetition exercises both event orderings of its race with Patch.
            for destination in [speakers, phones] * 8:
                node = next(s for s in json.loads(pactl("-f", "json", "list", "sinks"))
                            if s["name"] == destination)
                run(["omarchy-audio-output-set-default", node["properties"]["object.id"], destination])
            time.sleep(1.5)
        else:
            for destination in (phones, "patch", speakers, "patch", speakers, phones, speakers):
                before = time.monotonic()
                run([str(binary), "output", "set-default", destination])
                timings.append((time.monotonic() - before) * 1000)
                status = request(op="status")
                assert status["output"] == (destination if destination != "patch" else previous)
                previous = status["output"]
                assert status["system_volume"] == gain
                sinks = json.loads(pactl("-f", "json", "list", "sinks"))
                bus = next(sink["index"] for sink in sinks if sink["name"] == "patch")
                assert player()["sink"] == bus, "Player bypassed the mixer"
                assert pactl("get-default-sink") == "patch"
                time.sleep(0.15)
            # Repeated same-device selection must preserve mute and never blast.
            pactl("set-sink-mute", "patch", "1")
            wait(lambda: request(op="status")["system_volume"]["mute"])
            run([str(binary), "output", "set-default", speakers])
            assert request(op="status")["system_volume"]["mute"]
            pactl("set-sink-mute", "patch", "0")
            wait(lambda: not request(op="status")["system_volume"]["mute"])
            time.sleep(0.3)

        with capture_lock:
            measured = max(peak for chunks in captures.values() for at, peak in chunks if at >= marker)
        ratio = measured / expected_peak
        print(json.dumps({"baseline": baseline, "expected_peak": expected_peak,
                          "measured_peak": measured, "peak_ratio": ratio,
                          "loud_bypass_observed": ratio > 1.05,
                          "switch_ms": timings, "engine_dropped": request(op="status")["dropped"]}, indent=2))
        if not baseline:
            assert ratio < 1.05, "switching exceeded the listening-volume ceiling"
            assert max(timings) < 500, "destination selection is too slow"
            assert request(op="status")["dropped"] == 0
            modules = json.loads(pactl("-f", "json", "list", "modules"))
            assert sum(module["name"] == "module-loopback" for module in modules) <= 1
            # Inputs use a separate capture mix with no implicit speaker send.
            microphone = "alsa_input.routing-test-microphone"
            pactl("load-module", "module-null-sink", f"sink_name={microphone}",
                  "media.class=Audio/Source/Virtual", "channels=2", "rate=48000")
            wait(lambda: microphone in pactl("list", "short", "sources"))
            for side in ("FL", "FR"):
                run(["pw-link", f"Patch Player:output_{side}", f"{microphone}:input_{side}"])
            run([str(binary), "input", "set-default", microphone])
            assert pactl("get-default-source") == "patch.input"
            assert request(op="status")["mics"][0]["monitor"] is False
            request(op="gain", channel="mic-1", db=-6.0206)
            input_peaks = []
            input_reader = subprocess.Popen(["parec", "--raw", "--format=float32le", "--rate=48000",
                                             "--channels=2", "--device=patch.input"], env=env,
                                            stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)
            children.append(input_reader)

            def collect_input():
                while data := input_reader.stdout.read(4096):
                    samples = array.array("f")
                    samples.frombytes(data)
                    input_peaks.append(max(map(abs, samples), default=0))

            threading.Thread(target=collect_input, daemon=True).start()
            wait(lambda: input_peaks and 0.045 < input_peaks[-1] < 0.055)
            run([str(binary), "input", "set-default", "patch.input"])
            assert request(op="status")["mics"][0]["source"] == microphone
            pactl("set-sink-mute", "patch", "1")
            wait(lambda: request(op="status")["system_volume"]["mute"])
            time.sleep(0.15)
            assert 0.045 < input_peaks[-1] < 0.055, "speaker mute affected the input mix"
            run([str(binary), "input", "mix", "main"])
            wait(lambda: input_peaks and 0.09 < input_peaks[-1] < 0.11)
            assert request(op="input_mix")["main"]
            run([str(binary), "input", "mix", "inputs"])
            wait(lambda: 0.045 < input_peaks[-1] < 0.055)
            run([str(binary), "input", "monitor", "mic-1", "on"])
            assert request(op="status")["mics"][0]["monitor"]
            run([str(binary), "input", "monitor", "mic-1", "off"])
            if "--mic" in sys.argv[2:]:
                from mic_checks import verify
                verify(binary, run, request, root)
            input_reader.terminate()
            input_reader.wait(timeout=3)
            wait(lambda: "patch.engine:in_mic-1_" not in run(["pw-link", "-l"]))
            # A multichannel interface is expanded into independent mono
            # lanes, rather than collapsed into one stereo microphone.
            interface = "alsa_input.routing-test-interface"
            pactl("load-module", "module-null-sink", f"sink_name={interface}",
                  "media.class=Audio/Source/Virtual", "channels=16", "rate=48000",
                  "channel_map=" + ",".join(f"aux{i}" for i in range(16)))
            wait(lambda: interface in pactl("list", "short", "sources"))
            run([str(binary), "input", "set-default", interface])
            mics = request(op="status")["mics"]
            assert len(mics) == 16
            assert len({mic["source"] for mic in mics}) == 16
            assert all(not mic["monitor"] for mic in mics)
            reader = start(["parec", "--raw", "--device=patch.input"])
            def all_input_lanes_linked():
                graph = run(["pw-link", "-l"])
                return all(f"patch.engine:in_mic-{i}_FL" in graph for i in range(1, 17))

            wait(all_input_lanes_linked)
            reader.terminate()
            reader.wait(timeout=3)
            wait(lambda: "patch.engine:in_mic-" not in run(["pw-link", "-l"]))
            # Open modules appear immediately, close explicitly, and retain
            # an engine transport independently of any window.
            request(op="module_presence", module="sequencer", owner=123)
            assert "sequencer" in request(op="status")["visible_modules"]
            request(op="module_presence", module="sequencer", owner=123, open=False)
            assert "sequencer" not in request(op="status")["visible_modules"]
            print("Input capture, monitoring, privacy and module visibility: passed")
            if "--contextual" in sys.argv:
                from contextual_checks import check
                check(run, pactl, request, start, wait, tone, binary, speakers, env)
    finally:
        for process in reversed(children):
            if process.poll() is None:
                process.terminate()
                try:
                    process.wait(timeout=3)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait()
        for log in logs:
            log.close()
        if sys.exc_info()[0]:
            for log in root.glob("process-*.log"):
                print(log.name, log.read_text(errors="replace")[-2000:], file=sys.stderr)
