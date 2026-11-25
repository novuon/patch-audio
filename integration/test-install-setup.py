#!/usr/bin/env python3
"""install.sh keeps what is set up: docs/decisions.md, "An update keeps the setup".

Runs the real install.sh in a throwaway HOME with a fake binary and every command
that could touch the machine replaced by a stub (sudo, pacman, systemctl, pkill,
pgrep, hyprctl, tailscale, usermod, udevadm, apt-get, dnf, groupadd). The fake
gum presses Enter: a choice returns what the real one starts on, and an option
passed to --selected that isn't on offer fails the run, as gum would silently
drop it. Without gum (other distributions) the plain menus get Enter too.

Other distributions are played with install.sh's test hooks: PATCH_INSTALL_PM
(the package manager), PATCH_LIMITS_DIR (where the realtime limits go) and
PATCH_INSTALL_PLAIN (no gum, even where this machine has one).
"""
import os
import pathlib
import pty
import shutil
import stat
import subprocess
import sys
import tempfile

REPO = pathlib.Path(__file__).resolve().parent.parent
# The modules with a setup switch, as crates/patch-core/modules.json lists them.
ALL = "eq mic studio player dj ai_dj visualiser controllers plugins podcast live_coding themes".split()

FAKE_BIN = """#!/bin/bash
echo "patch $*" >> "$STUB_LOG"
case "$1" in
  --version) echo "omarchy-patch test" ;;
  status) echo closed ;;
esac
exit 0
"""
# The groups: `id -nG <user>` answers the group database (FAKE_DB_GROUPS),
# `id -nG` this session's groups (FAKE_SESSION_GROUPS); the rest is the real id.
FAKE_ID = """#!/bin/bash
if [[ $1 == -nG && -n ${2:-} ]]; then echo "${FAKE_DB_GROUPS:-tester}"; exit 0; fi
if [[ $1 == -nG ]]; then echo "${FAKE_SESSION_GROUPS:-tester}"; exit 0; fi
exec /usr/bin/id "$@"
"""
# Pretends nothing runs here, and that nothing can be changed.
STUB = """#!/bin/bash
echo "$(basename "$0") $*" >> "$STUB_LOG"
exit {code}
"""
FAKE_GUM = r"""#!/usr/bin/env python3
import os, sys
args = sys.argv[1:]
log = open(os.environ["STUB_LOG"], "a")
log.write("gum " + " ".join(args) + "\n")
if args[0] == "style":
    print(args[-1] if args else "")
    sys.exit(0)
if args[0] == "input":
    log.write("asked: key\n")
    print("")
    sys.exit(0)
if args[0] == "confirm":
    log.write("asked: " + args[-1] + "\n")
    sys.exit(0 if "--default=false" not in args else 1)
options, selected, multi, i = [], [], False, 1
while i < len(args):
    a = args[i]
    if a in ("--header", "--cursor", "--height"):
        i += 2
        continue
    if a == "--no-limit":
        multi = True
    elif a == "--selected":
        # gum splits on commas: a comma in an option never matches.
        selected += args[i + 1].split(",")
        i += 2
        continue
    else:
        options.append(a)
    i += 1
missing = [s for s in selected if s not in options]
if missing:
    log.write(f"gum: --selected not on offer: {missing}\n")
    sys.exit(3)
print("\n".join(selected) if multi else (selected[0] if selected else options[0]))
"""


def setup_toml(modules, layout, **core):
    lines = [f"{m} = {'true' if m in modules else 'false'}" for m in ALL]
    for name in ("system_audio", "start_with_session", "bar"):
        lines.append(f"{name} = {'true' if core.get(name, True) else 'false'}")
    lines.append(f'layout = "{layout}"')
    return "\n".join(lines) + "\n"


class Box:
    """A throwaway kit and HOME."""

    def __init__(self, root, omarchy=True, flavor=None, pm=None, plain=False):
        self.root = pathlib.Path(root)
        self.extra = {}
        self.kit = self.root / "kit"
        self.home = self.root / "home"
        self.stubs = self.root / "stubs"
        self.log = self.root / "log"
        for d in (self.kit / "bin", self.home, self.stubs):
            d.mkdir(parents=True)
        shutil.copy(REPO / "install.sh", self.kit / "install.sh")
        shutil.copy(REPO / "install-key.sh", self.kit / "install-key.sh")
        (self.kit / "VERSION").write_text("0.0.0\n")
        for sub in ("integration/desktop", "tools/stems", "themes/patch-light", "themes/patch-dark", "packaging"):
            shutil.copytree(REPO / sub, self.kit / sub)
        (self.kit / "crates/patch-core").mkdir(parents=True)
        shutil.copy(REPO / "crates/patch-core/modules.json", self.kit / "crates/patch-core/modules.json")
        self.write(self.kit / "bin/omarchy-patch", FAKE_BIN)
        for name, code in dict(
            sudo=0, pacman=1, systemctl=0, pkill=0, pgrep=1, hyprctl=1, tailscale=1, usermod=0,
            udevadm=0, getent=1, notify_send=0, update_desktop_database=0, clear=0,
        ).items():
            self.write(self.stubs / name.replace("_", "-"), STUB.format(code=code))
        self.write(self.stubs / "gum", FAKE_GUM)
        self.write(self.stubs / "id", FAKE_ID)
        self.config = self.home / ".config/omarchy-patch"
        self.config.mkdir(parents=True)
        self.installed = self.home / ".local/bin/omarchy-patch"
        if omarchy:
            (self.home / ".local/share/omarchy").mkdir(parents=True)
        if flavor:
            (self.kit / "FLAVOR").write_text(flavor + "\n")
        if pm:
            self.extra["PATCH_INSTALL_PM"] = pm
            self.extra["PATCH_LIMITS_DIR"] = str(self.root / "security")
            for name, code in {"apt-get": 0, "dnf": 0, "zypper": 0, "dpkg-query": 1, "rpm": 1, "groupadd": 0}.items():
                self.write(self.stubs / name, STUB.format(code=code))
        if plain:
            self.extra["PATCH_INSTALL_PLAIN"] = "1"

    @staticmethod
    def write(path, text):
        path.write_text(text)
        path.chmod(path.stat().st_mode | stat.S_IEXEC)

    def saved(self, *args, **kw):
        (self.config / "setup.toml").write_text(setup_toml(*args, **kw))

    def already_installed(self):
        self.installed.parent.mkdir(parents=True, exist_ok=True)
        self.write(self.installed, FAKE_BIN)

    def env(self):
        return {
            "PATH": f"{self.stubs}:{os.environ['PATH']}",
            "HOME": str(self.home),
            "USER": "tester",
            "STUB_LOG": str(self.log),
            "TERM": "dumb",
            "LANG": "C.UTF-8",
            "XDG_RUNTIME_DIR": str(self.root),
            # This machine may run Omarchy: only the box's own home says so.
            "PATCH_INSTALL_OMARCHY_DIR": "",
            **self.extra,
        }

    def run(self, *args, tty=False, deps=False, keys="\n" * 12, code_wanted=0):
        self.log.write_text("")
        cmd = ["bash", str(self.kit / "install.sh"), *([] if deps else ["--no-deps"]), "--no-remote", *args]
        if tty:
            parent, child = pty.openpty()
            p = subprocess.Popen(cmd, stdin=child, stdout=child, stderr=child, env=self.env(), close_fds=True)
            os.close(child)
            # Enter for every plain menu (gum's fake answers by itself).
            os.write(parent, keys.encode())
            out = b""
            try:
                while chunk := os.read(parent, 65536):
                    out += chunk
            except OSError:
                pass
            code = p.wait()
            os.close(parent)
            text = out.decode(errors="replace")
        else:
            r = subprocess.run(cmd, stdin=subprocess.DEVNULL, capture_output=True, text=True, env=self.env())
            code, text = r.returncode, r.stdout + r.stderr
            self.stdout = r.stdout
        assert code == code_wanted, f"install.sh exited {code}:\n{text}\n{self.log.read_text()}"
        self.output = text
        if code_wanted:
            return None, self.log.read_text()
        return self.result()

    def result(self):
        """What `setup set --all` was told, as {switch: value}, and the log."""
        log = self.log.read_text()
        line = next(l for l in log.splitlines() if l.startswith("patch setup set --all"))
        return dict(p.split("=", 1) for p in line.split()[4:]), log


def modules_on(setup):
    return {m for m in ALL if setup[m] == "on"}


def check(name, cond, detail=""):
    print(("ok   " if cond else "FAIL ") + name + ("" if cond else f"  {detail}"))
    return cond


def main():
    assert shutil.which("bash")
    ok = True

    def case(name, prepare, *args, tty=False, expect=None, box_args=None, deps=False):
        nonlocal ok
        with tempfile.TemporaryDirectory(prefix="patch-install-") as tmp:
            box = Box(tmp, **(box_args or {}))
            prepare(box)
            setup, log = box.run(*args, tty=tty, deps=deps)
            problems = expect(setup, log)
            ok &= check(name, not problems, f"{problems}\n{setup}")

    def update(modules, layout, **core):
        def prepare(box):
            box.already_installed()
            box.saved(modules, layout, **core)
        return prepare

    def want(modules, layout, stems=False, **core):
        def expect(setup, log):
            problems = []
            if modules_on(setup) != set(modules):
                problems.append(f"modules {sorted(modules_on(setup))}")
            if setup["layout"] != layout:
                problems.append(f"layout {setup['layout']}")
            for name in ("system_audio", "start_with_session", "bar"):
                value = "on" if core.get(name, True) else "off"
                if setup[name] != value:
                    problems.append(f"{name} {setup[name]}")
            stems_ran = "setup stems install" in log or any(
                l.startswith("patch modules add") and " stems" in l for l in log.splitlines())
            if stems_ran != stems:
                problems.append(f"stems download {'ran' if not stems else 'missing'}")
            if "sudo pacman" in log:
                problems.append("touched pacman")
            return problems
        return expect

    fresh = lambda box: None
    custom = ["dj", "plugins"]
    standard = ["eq", "themes"]
    dj = ["eq", "dj", "ai_dj", "themes"]
    # A first install with no terminal and no choice (an AI agent, a
    # script): the choices, and nothing installed (2026-11-25: an agent's
    # install quietly became Standard without asking the user).
    with tempfile.TemporaryDirectory(prefix="patch-install-") as tmp:
        box = Box(tmp)
        _, log = box.run(code_wanted=2)
        out = box.output
        listed = all(f"--profile {p}" in out for p in ("standard", "dj", "podcast", "live_coding", "full"))
        ok &= check("fresh, no terminal, no choice: lists the choices and stops",
                    listed and "--modules" in out and "ask the user" in out
                    and "setup set" not in log and "sudo" not in log, out[-800:] + log)
    case("fresh, no terminal, --profile standard: Standard", fresh, "--profile", "standard",
         expect=want(standard, "simple"))
    case("fresh, Enter through: Standard, its layout", fresh, tty=True, expect=want(standard, "simple"))
    case("fresh, --profile dj: decks and the AI DJ, no stems download", fresh, "--profile", "dj",
         expect=want(dj, "ai-dj"))

    def key_expect(setup, log):
        problems = want(dj, "ai-dj")(setup, log)
        if "patch licence activate PATCH-TEST" not in log:
            problems.append("key not activated")
        return problems
    case("--key activates the key", fresh, "--profile", "dj", "--key", "PATCH-TEST", expect=key_expect)
    case("update Full + Pro, Enter through: kept, no download", update(ALL, "pro"), tty=True,
         expect=want(ALL, "pro"))
    case("update Full + Pro, no terminal: kept, no download", update(ALL, "pro"),
         expect=want(ALL, "pro"))
    case("update Minimal + Simple, Enter through: kept", update([], "simple"), tty=True,
         expect=want([], "simple"))
    case("update Custom (a module text with a comma), Enter through: kept", update(custom, "basic-dj"), tty=True,
         expect=want(custom, "basic-dj"))
    case("update every Custom module, Enter through: kept", update(ALL[:-1], "pro"), tty=True,
         expect=want(ALL[:-1], "pro"))
    case("update Custom, no terminal: kept", update(custom, "basic-dj"),
         expect=want(custom, "basic-dj"))
    case("bar off, Enter through: stays off", update(ALL, "pro", bar=False), tty=True,
         expect=want(ALL, "pro", bar=False))
    case("login service off, no terminal: stays off", update(ALL, "pro", start_with_session=False),
         expect=want(ALL, "pro", start_with_session=False))
    case("system audio off, no terminal: stays off", update(ALL, "pro", system_audio=False),
         expect=want(ALL, "pro", system_audio=False))

    def after_uninstall(box):
        box.saved(ALL, "pro", system_audio=False)
    case("reinstall after uninstall: system audio back on, modules kept", after_uninstall,
         expect=want(ALL, "pro", stems=False))

    case("explicit --no-bar wins", update(ALL, "pro"), "--no-bar", expect=want(ALL, "pro", bar=False))
    case("explicit --profile dj wins, the layout stays when offered", update(ALL, "basic-dj"),
         "--profile", "dj", expect=want(dj, "basic-dj"))
    case("a layout the modules no longer offer falls back", update(ALL, "ai-dj"),
         "--profile", "minimal", expect=want([], "simple"))

    def stems_installed(box):
        update(ALL, "pro")(box)
        (box.config / "stems.json").write_text("{}\n")

    def stems_expect(setup, log):
        problems = want(ALL, "pro", stems=True)(setup, log)
        if "asked: Download" in log:
            problems.append("asked again")
        return problems
    case("stems installed: refreshed without asking", stems_installed, tty=True, expect=stems_expect)

    # A 0.7.x setup.toml knows only the older switches: EQ, Mic, Studio tools
    # and the Player were core Units then, and stay on.
    OLD = "dj ai_dj visualiser podcast live_coding plugins controllers themes".split()

    def old_setup(on):
        def prepare(box):
            box.already_installed()
            lines = [f"{m} = {'true' if m in on else 'false'}" for m in OLD]
            lines += ["system_audio = true", "start_with_session = true", "bar = true", 'layout = "simple"']
            (box.config / "setup.toml").write_text("\n".join(lines) + "\n")
        return prepare
    new_core = ["eq", "mic", "studio", "player"]
    case("update from 0.7.x Minimal, no terminal: the Units it showed stay", old_setup([]),
         expect=want(new_core, "simple"))
    case("update from 0.7.x Minimal, Enter through: the Units it showed stay", old_setup([]), tty=True,
         expect=want(new_core, "simple"))
    case("update from 0.7.x Full, Enter through: everything stays", old_setup(OLD), tty=True,
         expect=want(ALL, "simple"))

    # ── Other distributions ────────────────────────────────────────────
    def packages(pm, omarchy=False, **extra_stubs):
        with tempfile.TemporaryDirectory(prefix="patch-install-") as tmp:
            box = Box(tmp, omarchy=omarchy, flavor="omarchy" if omarchy else "generic", pm=pm)
            for name, text in extra_stubs.items():
                box.write(box.stubs / name, text)
            r = subprocess.run(["bash", str(box.kit / "install.sh"), "--print-packages"],
                               capture_output=True, text=True, env=box.env())
            assert r.returncode == 0, r.stderr
            return r.stdout.split()

    apt = packages("apt-get")
    ok &= check("Ubuntu/Debian: apt names (PipeWire tools, pactl), no Arch-only packages",
                {"pipewire", "pipewire-bin", "pulseaudio-utils", "ffmpeg"} <= set(apt)
                and not {"realtime-privileges", "libpulse", "rubberband", "uv"} & set(apt), apt)
    dnf = packages("dnf")
    ok &= check("Fedora without RPM Fusion: ffmpeg-free", "ffmpeg-free" in dnf and "ffmpeg" not in dnf, dnf)
    fusion = packages("dnf", dnf='#!/bin/bash\n[[ $1 == repolist ]] && echo "rpmfusion-free RPM Fusion"\nexit 0\n')
    ok &= check("Fedora with RPM Fusion: ffmpeg", "ffmpeg" in fusion and "ffmpeg-free" not in fusion, fusion)
    ok &= check("openSUSE: zypper names", {"pipewire-tools", "libnotify-tools"} <= set(packages("zypper")))
    omarchy_list = "pipewire libpulse wireplumber ffmpeg rubberband sqlite fontconfig libnotify xdg-user-dirs realtime-privileges uv python".split()
    ok &= check("Arch: the Omarchy list, unchanged", packages("pacman", omarchy=True) == omarchy_list)
    # rtkit gives every PipeWire app's audio thread a 200 ms CPU limit and
    # the kernel kills the app past it (integration/test-rt-clients.py).
    every = {pm: packages(pm) for pm in ("pacman", "apt-get", "dnf", "zypper")}
    ok &= check("No rtkit on any system (it kills other apps' busy audio threads)",
                not any("rtkit" in names for names in every.values()), every)
    arch = packages("pacman")
    ok &= check("Arch without Omarchy: the windowing libraries too",
                arch[:len(omarchy_list)] == omarchy_list and {"libxkbcommon", "libglvnd"} <= set(arch), arch)

    generic = dict(omarchy=False, flavor="generic", pm="apt-get")

    def apt_expect(setup, log):
        problems = want(standard, "simple")(setup, log)
        if not any(l.startswith("sudo apt-get install -y") and " pipewire-bin" in l for l in log.splitlines()):
            problems.append("apt didn't install the runtime")
        if "sudo groupadd -r realtime" not in log or "95-patch-realtime.conf" not in log:
            problems.append("no realtime group and limits")
        if "sudo pacman" in log or any(l.startswith("curl ") for l in log.splitlines()):
            problems.append("pacman or a download")
        return problems
    case("Ubuntu, generic kit: apt packages, realtime limits, Standard", fresh, "--profile", "standard",
         box_args=generic, deps=True, expect=apt_expect)
    case("Ubuntu, no gum, Enter through the plain menus: Standard, its layout", fresh, tty=True,
         box_args=dict(generic, plain=True), expect=want(standard, "simple"))
    case("Ubuntu, no gum, update Full + Pro, Enter through: kept", update(ALL, "pro"), tty=True,
         box_args=dict(generic, plain=True), expect=want(ALL, "pro"))

    # The re-login step: in the realtime group, but this session started
    # before, so real-time only from the next login. Shown on every run
    # until then, as the last thing on screen (after the guide).
    step = "One step left: log out and back in"
    for name, db, session, shown in (
        ("just added to the realtime group: the log-out step, last", "tester realtime", "tester", True),
        ("run again before logging out: the log-out step again", "tester realtime", "tester", True),
        ("logged in again since: no log-out step", "tester realtime", "tester realtime", False),
        ("no realtime group (--no-deps): no log-out step", "tester", "tester", False),
    ):
        with tempfile.TemporaryDirectory(prefix="patch-install-") as tmp:
            box = Box(tmp)
            box.extra.update(FAKE_DB_GROUPS=db, FAKE_SESSION_GROUPS=session)
            box.run("--profile", "standard")
            out = box.stdout.rstrip()
            last = out[out.rfind(step):] if step in out else ""
            ok &= check(name, (step in out) == shown and (not shown or (
                "Once is enough" in last and out.endswith("Once is enough."))), out[-600:])

    def fetches_generic(setup, log):
        problems = want(standard, "simple")(setup, log)
        if "linux-generic" not in log:
            problems.append("the Omarchy kit on Ubuntu didn't fetch the generic build")
        return problems

    def omarchy_kit_on_ubuntu(box):
        box.write(box.stubs / "curl", STUB.format(code=1))
    case("the Omarchy kit on Ubuntu: asks for the generic build, falls back to its own", omarchy_kit_on_ubuntu,
         "--profile", "standard",
         box_args=dict(omarchy=False, flavor="omarchy", pm="apt-get"), expect=fetches_generic)

    print("install.sh keeps the setup" if ok else "FAILED")
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
