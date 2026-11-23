#!/usr/bin/env python3
"""Close every Patch UI window, including windows from an unresponsive session."""
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import time


def clients():
    result = subprocess.run(
        ["hyprctl", "clients", "-j"], capture_output=True, text=True, timeout=3
    )
    if result.returncode:
        raise RuntimeError(result.stderr.strip() or "hyprctl clients failed")
    return [c for c in json.loads(result.stdout) if c.get("class") == "omarchy.patch"]


def patch_ui_pid(pid):
    try:
        return Path(os.readlink(f"/proc/{pid}/exe").removesuffix(" (deleted)")).name == "omarchy-patch"
    except (OSError, ValueError):
        return False


def main():
    before = clients()
    # A window-owning UI process can remain alive after its last window has
    # been closed by Hyprland. Remember these verified UI processes so a new
    # layout cannot be routed to an old, stale session over its control socket.
    ui_pids = {
        int(c["pid"])
        for c in before
        if str(c.get("pid", "")).isdigit() and patch_ui_pid(int(c["pid"]))
    }
    for client in before:
        address = client.get("address")
        if address:
            subprocess.run(
                ["hyprctl", "dispatch", "closewindow", f"address:{address}"],
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
                timeout=3,
            )

    deadline = time.monotonic() + 0.6
    remaining = clients()
    while remaining and time.monotonic() < deadline:
        time.sleep(0.05)
        remaining = clients()

    for pid in ui_pids:
        try:
            os.kill(pid, signal.SIGTERM)
        except ProcessLookupError:
            pass

    deadline = time.monotonic() + 1.2
    while (
        remaining or any(Path(f"/proc/{pid}").exists() for pid in ui_pids)
    ) and time.monotonic() < deadline:
        time.sleep(0.05)
        remaining = clients()

    # A Patch UI process can be stuck in its renderer. It owns no audio engine;
    # force-stop only the verified UI processes whose windows were just closed.
    survivors = {pid for pid in ui_pids if Path(f"/proc/{pid}").exists()}
    for pid in survivors:
        if patch_ui_pid(pid):
            try:
                os.kill(pid, signal.SIGKILL)
            except ProcessLookupError:
                pass

    deadline = time.monotonic() + 1.0
    while (
        remaining or any(Path(f"/proc/{pid}").exists() for pid in survivors)
    ) and time.monotonic() < deadline:
        time.sleep(0.05)
        remaining = clients()
    if remaining or any(Path(f"/proc/{pid}").exists() for pid in survivors):
        print(f"Could not close {len(remaining)} Patch window(s).", file=sys.stderr)
        return 1
    print(f"Closed {len(before)} Patch window(s).")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (OSError, RuntimeError, json.JSONDecodeError, subprocess.SubprocessError) as error:
        print(f"Patch window cleanup failed: {error}", file=sys.stderr)
        raise SystemExit(1)
