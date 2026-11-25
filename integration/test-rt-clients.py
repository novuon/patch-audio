#!/usr/bin/env python3
"""Other apps' audio survives Patch's real-time setup.

Builds integration/rt-busy-client.c: a PipeWire client whose audio (data)
thread runs 1 s of CPU without blocking, as cliamp's did. PipeWire's
module-rt makes that thread real-time. Through rtkit (installed by Patch
0.8.7 and earlier) it gets rtkit's RLIMIT_RTTIME of 200000/200000 µs, and the
kernel SIGKILLs the app after 200 ms: exit 137. Through the realtime group's
limits (realtime-privileges, from the next login) it gets no CPU limit.

Two cases, run as this user:

  before re-login  RLIMIT_RTPRIO lowered to 0, as before the realtime group's
                   limits apply: module-rt falls back to the portal or rtkit.
  after re-login   this session's own limits; needs the realtime group in
                   force (RLIMIT_RTPRIO of 11 or more), else it is skipped.

Both must survive. Run it after installing Patch, before and after logging
in again. Never joins the audio graph (no stream is connected), so it is
safe next to live audio; it uses one core for about a second per case.

  python3 integration/test-rt-clients.py
"""
import os
import resource
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

HERE = Path(__file__).resolve().parent


def build(out):
    if not shutil.which("cc") or not shutil.which("pkg-config"):
        return "no C compiler or pkg-config"
    flags = subprocess.run(["pkg-config", "--cflags", "--libs", "libpipewire-0.3"],
                           capture_output=True, text=True)
    if flags.returncode:
        return "no libpipewire-0.3 headers (pkg-config)"
    r = subprocess.run(["nice", "-n", "19", "cc", "-O1", "-Wall", str(HERE / "rt-busy-client.c"),
                        *flags.stdout.split(), "-o", str(out)], capture_output=True, text=True)
    if r.returncode:
        sys.exit(f"rt-busy-client didn't build:\n{r.stderr}")
    return None


def rtkit_present():
    r = subprocess.run(["busctl", "--system", "status", "org.freedesktop.RealtimeKit1"],
                       capture_output=True, text=True) if shutil.which("busctl") else None
    return bool(r and r.returncode == 0)


def run(name, cmd):
    r = subprocess.run(cmd, capture_output=True, text=True, timeout=30)
    out = (r.stdout + r.stderr).strip()
    if r.returncode == 0:
        print(f"ok    {name}: {out.splitlines()[0] if out else ''}")
        return True
    why = "SIGKILL (RLIMIT_RTTIME)" if r.returncode in (-9, 137) else f"exit {r.returncode}"
    print(f"FAIL  {name}: {why}\n      {out}")
    if rtkit_present():
        print("      rtkit is running on this system: an app's audio thread gets its "
              "200 ms limit until the realtime group's limits apply.\n"
              "      Patch no longer installs rtkit; one an older Patch installed stays "
              "(sudo pacman -R rtkit removes it, if nothing else needs it).")
    return False


def main():
    with tempfile.TemporaryDirectory() as tmp:
        client = Path(tmp) / "rt-busy-client"
        skip = build(client)
        if skip:
            print(f"skipped: {skip}")
            return 0
        ok = run("before re-login (no realtime limits)", ["prlimit", "--rtprio=0", str(client)])
        _, hard = resource.getrlimit(resource.RLIMIT_RTPRIO)
        if hard == resource.RLIM_INFINITY or hard >= 11:
            ok &= run("after re-login (realtime group)", [str(client)])
        else:
            print(f"skip  after re-login: RLIMIT_RTPRIO is {hard} here; "
                  "run again once you have logged in after installing Patch")
        return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
