#!/usr/bin/env python3
"""Drive the real Void desktop app on macOS the way a user would, and capture
screenshots of its window (never the rest of the screen).

    scripts/drive.py <app-binary> <out-dir> [scenario ...]

Each run seeds a fresh sandbox home with `void dev seed`, writes the theme and
density into its settings, launches the app with VOID_HOME pointing at it,
sends keystrokes and clicks through System Events / cliclick, and saves
`<out-dir>/<scenario>-<theme>-<density>-<step>.png`.

Needs: macOS, python3 with pyobjc (Quartz), cliclick, the `void` CLI built
(target/debug/void), and Accessibility permission for the terminal.
"""

import json
import os
import shutil
import subprocess
import sys
import tempfile
import time

import Quartz

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
VOID_CLI = os.path.join(ROOT, "target", "debug", "void")


def windows_of(pid):
    wins = Quartz.CGWindowListCopyWindowInfo(
        Quartz.kCGWindowListOptionOnScreenOnly | Quartz.kCGWindowListExcludeDesktopElements,
        Quartz.kCGNullWindowID,
    )
    return [w for w in wins if w.get("kCGWindowOwnerPID") == pid and w.get("kCGWindowLayer") == 0]


class App:
    def __init__(self, binary, home, width=900, height=700):
        self.binary = binary
        self.home = home
        self.size = (width, height)
        self.proc = None

    def launch(self):
        env = dict(os.environ, VOID_HOME=self.home)
        self.proc = subprocess.Popen([self.binary], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        for _ in range(60):
            time.sleep(0.5)
            if windows_of(self.proc.pid):
                break
        else:
            raise SystemExit("app window never appeared")
        time.sleep(1.5)
        self.front()
        self.resize(*self.size)

    def front(self):
        osa(f'tell application "System Events" to set frontmost of (first process whose unix id is {self.proc.pid}) to true')
        time.sleep(0.3)

    def resize(self, w, h):
        osa(
            f'tell application "System Events" to tell (first process whose unix id is {self.proc.pid}) '
            f"to set size of window 1 to {{{w}, {h}}}"
        )
        osa(
            f'tell application "System Events" to tell (first process whose unix id is {self.proc.pid}) '
            f"to set position of window 1 to {{80, 80}}"
        )
        time.sleep(0.6)

    def bounds(self):
        w = max(windows_of(self.proc.pid), key=lambda w: w["kCGWindowBounds"]["Width"])
        b = w["kCGWindowBounds"]
        return w["kCGWindowNumber"], int(b["X"]), int(b["Y"]), int(b["Width"]), int(b["Height"])

    def shot(self, path, settle=0.6):
        time.sleep(settle)
        wid = self.bounds()[0]
        subprocess.run(["screencapture", "-x", "-o", "-l", str(wid), path], check=True)
        print("  captured", os.path.basename(path))

    def key(self, key, *mods, settle=0.4):
        """key: a character, or a name like 'return', 'escape', 'down', 'up', 'space', 'tab'."""
        self.front()
        codes = {"return": 36, "escape": 53, "down": 125, "up": 126, "left": 123, "right": 124, "space": 49, "tab": 48}
        using = ""
        if mods:
            using = " using {" + ", ".join(f"{m} down" for m in mods) + "}"
        if key in codes:
            osa(f'tell application "System Events" to key code {codes[key]}{using}')
        else:
            osa(f'tell application "System Events" to keystroke "{key}"{using}')
        time.sleep(settle)

    def type(self, text, settle=0.4):
        self.front()
        osa(f'tell application "System Events" to keystroke "{text}"')
        time.sleep(settle)

    def click(self, x, y, settle=0.5):
        """Click at window-relative point (x, y) in points."""
        _, wx, wy, _, _ = self.bounds()
        subprocess.run(["cliclick", f"c:{wx + x},{wy + y}"], check=True)
        time.sleep(settle)

    def quit(self):
        if self.proc:
            self.proc.terminate()
            try:
                self.proc.wait(5)
            except subprocess.TimeoutExpired:
                self.proc.kill()


def osa(script):
    subprocess.run(["osascript", "-e", script], check=True, stdout=subprocess.DEVNULL)


def seed(theme, density, extra=None):
    home = tempfile.mkdtemp(prefix="void-e2e-")
    subprocess.run([VOID_CLI, "dev", "seed", home], check=True, stdout=subprocess.DEVNULL)
    cfg_dir = os.path.join(home, ".void-sandbox-config")
    cfg_path = os.path.join(cfg_dir, "config.json")
    with open(cfg_path) as f:
        cfg = json.load(f)
    cfg["ui"]["theme"] = theme
    cfg["ui"]["density"] = density
    # A second tray icon during tests is noise next to the user's own Void.
    cfg["ui"]["show_menu_bar_icon"] = False
    for k, v in (extra or {}).items():
        cfg[k] = v
    with open(cfg_path, "w") as f:
        json.dump(cfg, f, indent=2)
    return home


# ── Scenarios ────────────────────────────────────────────────────────────────
# Keyboard first: the shortcuts are part of the spec and identical in both
# builds, so the same scenario drives the Tauri baseline and the GPUI app.


def screens(app, out):
    app.shot(out("01-results-empty"))
    app.key("r", "command")
    app.shot(out("02-results-scanning"), settle=0.2)
    time.sleep(6)
    app.shot(out("03-results"))
    app.key("down")
    app.key("down")
    app.key("return")
    app.shot(out("04-drawer"))
    app.key("escape")
    app.key("a", "command", "option")
    app.shot(out("05-selected-safe"))
    app.key("return", "command")
    app.shot(out("06-confirm"))
    app.key("escape")
    app.key("a", "command")
    app.key("return", "command")
    app.shot(out("07-confirm-danger"))
    app.key("escape")
    app.key("a", "command", "shift")
    app.key("2", "command")
    time.sleep(1.5)
    app.shot(out("08-agents"))
    app.key("3", "command")
    app.shot(out("09-history-empty"))
    app.key("4", "command")
    app.shot(out("10-settings"))
    app.key("1", "command")
    app.key("d", "command")
    app.shot(out("11-results-toggled-density"))
    app.key("d", "command")


def clean(app, out):
    app.key("r", "command")
    time.sleep(6)
    app.key("a", "command", "option")
    app.key("return", "command")
    app.key("return")  # nothing focused in the modal; the button is clicked below if needed
    app.shot(out("01-confirm"))


SCENARIOS = {"screens": screens, "clean": clean}


def main():
    if len(sys.argv) < 3:
        print(__doc__)
        sys.exit(2)
    binary, out_dir = sys.argv[1], sys.argv[2]
    names = sys.argv[3:] or ["screens"]
    combos = os.environ.get("COMBOS", "light:comfortable,dark:comfortable,light:compact,dark:compact")
    os.makedirs(out_dir, exist_ok=True)
    for combo in combos.split(","):
        theme, density = combo.split(":")
        for name in names:
            print(f"{name} · {theme} · {density}")
            home = seed(theme, density)
            app = App(binary, home)
            try:
                app.launch()
                SCENARIOS[name](app, lambda step: os.path.join(out_dir, f"{name}-{theme}-{density}-{step}.png"))
            finally:
                app.quit()
                shutil.rmtree(home, ignore_errors=True)


if __name__ == "__main__":
    main()
