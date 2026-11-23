"""Extra checks on the base test's private PipeWire server; no hardware access."""
import time
import array
import subprocess
import json


def check(run, pactl, request, start, wait, tone, binary, speakers, env):
    output_interface = "alsa_output.routing-test-interface"
    pactl("load-module", "module-null-sink", f"sink_name={output_interface}", "channels=8", "rate=48000", "channel_map=" + ",".join(f"aux{i}" for i in range(8)))
    wait(lambda: output_interface in pactl("list", "short", "sinks"))
    for application in ["Browser", "Browser", "X", "Spotify"]:
        start(["mpv", "--no-config", "--no-video", "--really-quiet", "--ao=pipewire", "--audio-device=pipewire/patch", f"--audio-client-name={application}", "--volume=100", "--loop-file=inf", str(tone)])
    time.sleep(0.4)
    pactl("set-sink-mute", "patch", "0")
    wait(lambda: not request(op="status")["system_volume"]["mute"])
    print(run([str(binary.parent / "examples" / "contextual-check")]))
    main_ports = [f"{output_interface}:playback_AUX6", f"{output_interface}:playback_AUX7"]
    cue_ports = [f"{output_interface}:playback_AUX2", f"{output_interface}:playback_AUX3"]
    assert request(op="output_route", bus="cue", sink=output_interface, ports=cue_ports) == "ok"
    graph = run(["pw-link", "-l"])
    assert all(port in graph for port in main_ports + cue_ports)
    assert request(op="status")["output_ports"] == main_ports
    assert request(op="status")["cue_ports"] == cue_ports
    # Reject a cue assignment that would sum into the Main ports.
    try:
        request(op="output_route", bus="cue", sink=output_interface, ports=main_ports)
    except AssertionError:
        pass
    else:
        raise AssertionError("overlapping Main/Cue routes accepted")
    assert request(op="status")["cue_ports"] == cue_ports
    request(op="pfl", channel="bus", on=True)
    request(op="cue", mix=0, level_db=-20)
    time.sleep(0.2)

    def measured_ports():
        capture = subprocess.Popen(["parec", "--raw", "--format=float32le", "--rate=48000", "--channels=8", "--channel-map=" + ",".join(f"aux{i}" for i in range(8)), f"--device={output_interface}.monitor"], env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        try:
            try:
                capture.communicate(timeout=0.8)
                raise AssertionError("capture exited unexpectedly")
            except subprocess.TimeoutExpired:
                capture.terminate()
                data, errors = capture.communicate(timeout=3)
            assert len(data) >= 4096, errors.decode(errors="replace")
        finally:
            if capture.poll() is None:
                capture.kill()
                capture.wait()
        samples = array.array("f")
        samples.frombytes(data[:len(data) // 32 * 32])
        return [max(map(abs,samples[i::8]), default=0) for i in range(8)]

    levels = measured_ports()
    assert max(levels[6:8]) > 1e-5, levels
    assert max(levels[2:4]) > 1e-5, levels
    assert max(levels[i] for i in [0,1,4,5]) < 1e-6, levels
    pactl("set-sink-mute", "patch", "1")
    wait(lambda: request(op="status")["system_volume"]["mute"])
    time.sleep(0.2)
    muted = measured_ports()
    assert max(muted[6:8]) < 1e-6 and max(muted[2:4]) > 1e-5, muted
    pactl("set-sink-mute", "patch", "0")
    wait(lambda: not request(op="status")["system_volume"]["mute"])
    print(json.dumps({"eight_port_peaks":levels,"main_muted_peaks":muted}))
    request(op="pfl", channel="bus", on=False)
    gain = request(op="status")["system_volume"]
    run([str(binary), "output", "set-default", speakers])
    assert request(op="status")["system_volume"] == gain
    graph = run(["pw-link", "-l"])
    assert "patch.engine:cue_FL\n  |->" not in graph
    run([str(binary), "output", "set-default", "patch"])
    assert request(op="output_route", bus="main", sink=output_interface, ports=main_ports) == "ok"
    assert request(op="status")["system_volume"] == gain
    print("Generic 8-output interface, distinct Main/Cue pairs and mode switching: passed")
    status = request(op="status")
    assert status["dropped"] == 0
    print(json.dumps({"callback_avg_ns": status["cost_avg_ns"], "callback_max_ns": status["cost_max_ns"], "engine_dropped":status["dropped"]}))
