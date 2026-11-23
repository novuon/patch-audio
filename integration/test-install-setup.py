#!/usr/bin/env python3
"""install.sh keeps what is set up: docs/decisions.md, "An update keeps the setup".

Runs the real install.sh in a throwaway HOME with a fake binary and every command
that could touch the machine replaced by a stub (sudo, pacman, systemctl, pkill,
pgrep, hyprctl, tailscale, usermod, udevadm). The fake gum presses Enter: a
choice returns what the real one starts on, and an option passed to --selected
that isn't on offer fails the run, as gum would silently drop it.
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

    def __init__(self, root):
        self.root = pathlib.Path(root)
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
        self.config = self.home / ".config/omarchy-patch"
        self.config.mkdir(parents=True)
        self.installed = self.home / ".local/bin/omarchy-patch"

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
        }

    def run(self, *args, tty=False):
        self.log.write_text("")
        cmd = ["bash", str(self.kit / "install.sh"), "--no-deps", "--no-remote", *args]
        if tty:
            parent, child = pty.openpty()
            p = subprocess.Popen(cmd, stdin=child, stdout=child, stderr=child, env=self.env(), close_fds=True)
            os.close(child)
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
        assert code == 0, f"install.sh exited {code}:\n{text}\n{self.log.read_text()}"
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

    def case(name, prepare, *args, tty=False, expect=None):
        nonlocal ok
        with tempfile.TemporaryDirectory(prefix="patch-install-") as tmp:
            box = Box(tmp)
            prepare(box)
            setup, log = box.run(*args, tty=tty)
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
    standard = ["eq"]
    dj = ["eq", "dj", "ai_dj"]
    case("fresh, no terminal: Standard", fresh, expect=want(standard, "simple"))
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

    print("install.sh keeps the setup" if ok else "FAILED")
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
