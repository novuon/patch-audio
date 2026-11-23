"""Mic checks run only inside test-core-routing's private audio servers."""
import json


def verify(binary, run, request, root):
    profiles = json.loads(run([str(binary), "mic", "profiles"]))
    assert len(profiles) == 9
    assert {p["name"] for p in profiles} >= {"Natural", "Broadcast", "Noise reduction", "Custom"}
    previous = request(op="mic_profile", action="snapshot", channel="mic-1")
    previous["name"] = "Integration previous"
    request(op="mic_profile", action="import", profile=previous)
    initial = request(op="status")
    source = initial["mics"][0]["source"]
    choices = request(op="sources")
    assert sum(p.get("device") == source for p in choices) >= 2, "stereo capture ports missing"
    meter = request(op="mics")["mics"][0]["meter"]
    assert meter and 0.09 < meter["input_peak"] < 0.11
    history = request(op="mic_history", channel="mic-1")
    assert history["channel"] == "mic-1" and 0 < len(history["peaks"]) <= 128
    assert history["start_nsec"] <= history["end_nsec"]
    request(op="mic_profile", channel="mic-1", name="Broadcast")
    current = request(op="mic_profile", action="snapshot", channel="mic-1")
    assert abs(current["voice"]["comp"] - 0.7) < 1e-6
    assert current["eq"]["bands"][1]["gain_db"] == 2.0
    # Loading a portable profile preserves source and monitoring policy.
    assert request(op="status")["mics"][0]["source"] == initial["mics"][0]["source"]
    assert request(op="status")["mics"][0]["monitor"] is False
    request(op="mic_profile", action="restore", channel="mic-1")
    restored = request(op="mic_profile", action="snapshot", channel="mic-1")
    assert restored["voice"] == previous["voice"]
    assert restored["eq"]["bands"] == previous["eq"]["bands"]
    run([str(binary), "mic", "duplicate", "Natural", "Integration duplicate"])
    profile = root / "profile.json"
    run([str(binary), "mic", "export", "Integration duplicate", str(profile)])
    imported = json.loads(profile.read_text())
    imported["name"] = "Integration imported"
    profile.write_text(json.dumps(imported))
    run([str(binary), "mic", "import", str(profile)])
    run([str(binary), "mic", "set", "mic-1", "expander", "-50"])
    run([str(binary), "mic", "set", "mic-1", "noise", "-55"])
    run([str(binary), "mic", "save", "mic-1", "Integration edited"])
    saved = request(op="mic_profile", action="export", name="Integration edited")
    assert saved["voice"]["expander_db"] == -50
    assert saved["voice"]["noise_db"] == -55
    request(op="mic_profile", channel="mic-1", name="Integration previous")
    print("Mic profiles: factory, apply/restore, duplicate, import/export, save, shared voice controls passed")
