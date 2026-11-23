#!/usr/bin/env python3
"""Read-only Auto Mix capture for Visualiser's production-compositor replay test.

Run while the Auto Mix is already mixing. Never starts playback or changes a plan.
"""
import argparse
import json
import os
from pathlib import Path
import socket
import time


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("output", type=Path)
    parser.add_argument("--timeout", type=float, default=600)
    args = parser.parse_args()
    if not 1 <= args.timeout <= 3600:
        parser.error("timeout must be between 1 and 3600 seconds")
    endpoint = Path(os.environ["XDG_RUNTIME_DIR"]) / "omarchy-patch/engine.sock"
    samples = []
    started = False
    completed_at = None
    deadline = time.monotonic() + args.timeout
    while time.monotonic() < deadline:
        with socket.socket(socket.AF_UNIX) as connection:
            connection.settimeout(2)
            connection.connect(str(endpoint))
            connection.sendall(b'{"op":"ai_state"}\n')
            with connection.makefile() as reply:
                value = json.loads(reply.readline())
        if "err" in value:
            raise RuntimeError(value["err"])
        state = value.get("ok", value)
        transition = state.get("transition")
        if transition or started:
            samples.append(state)
        if transition and transition.get("started") and not started:
            started = True
            print("Observed actual Auto Mix transition", flush=True)
        if started and not transition and completed_at is None:
            completed_at = time.monotonic()
        if completed_at is not None and time.monotonic() - completed_at >= 2:
            break
        time.sleep(0.25 if transition or started else 1)
    args.output.parent.mkdir(parents=True, exist_ok=True)
    temporary = args.output.with_suffix(".tmp")
    temporary.write_text(json.dumps(samples))
    temporary.replace(args.output)
    print(f"Saved {len(samples)} stamped states to {args.output}")
    if not started or completed_at is None:
        raise SystemExit("No complete transition captured within timeout")


if __name__ == "__main__":
    main()
