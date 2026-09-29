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



def pixels(path):
    """RGBA pixel reader for a screenshot, at the capture's pixel scale."""
    from AppKit import NSBitmapImageRep
    rep = NSBitmapImageRep.imageRepWithContentsOfFile_(path)
    return rep


def find_color(path, rgb, tol=24, step=3):
    """Centre (in window points) of the pixels close to `rgb`, or None."""
    rep = pixels(path)
    w, h = rep.pixelsWide(), rep.pixelsHigh()
    xs, ys = [], []
    for y in range(0, h, step):
        for x in range(0, w, step):
            c = rep.colorAtX_y_(x, y)
            r, g, b = (int(c.redComponent() * 255), int(c.greenComponent() * 255), int(c.blueComponent() * 255))
            if abs(r - rgb[0]) <= tol and abs(g - rgb[1]) <= tol and abs(b - rgb[2]) <= tol:
                xs.append(x)
                ys.append(y)
    if len(xs) < 20:
        return None
    xs.sort()
    ys.sort()
    scale = w / 900 if w > 1000 else 1
    return (xs[len(xs) // 2] / scale, ys[len(ys) // 2] / scale)


def clipboard():
    return subprocess.run(["pbpaste"], capture_output=True, text=True).stdout


# Window-relative click targets (points) in the GPUI build at 900x700, with the
# macOS title bar (28pt) included. Measured from its screenshots.
SETTINGS_TABS = {
    "Scanning": 112, "Ecosystems": 198, "AI": 263, "Guard": 311,
    "Safety": 372, "Performance": 453, "General": 537, "About": 601,
}
TAB_Y = 105
DANGER = {"light": (0xBF, 0x21, 0x45), "dark": (0xEF, 0x54, 0x6C)}


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




def matrix(app, out, theme="dark"):
    """The functional matrix, as a user would run it. Screenshots every step;
    assertions print FAIL lines rather than stopping the run."""
    results = []

    def check(name, ok, detail=""):
        results.append((name, ok))
        print(("  PASS " if ok else "  FAIL ") + name + (f" — {detail}" if detail and not ok else ""))

    app.shot(out("m01-empty"))
    app.key("r", "command")
    time.sleep(6)
    app.shot(out("m02-results"))

    # Group and sort chips cycle.
    for i, label in enumerate(["project", "risk", "ecosystem"]):
        app.click(457, 167)
        app.shot(out(f"m03-group-{label}"))
    for i, label in enumerate(["stale", "name", "risk", "size"]):
        app.click(570, 167)
        app.shot(out(f"m04-sort-{label}"))

    # Filters chip: large items only (the sandbox has none) → "No items match".
    app.click(319, 167)
    app.shot(out("m05-large-only"))
    app.click(319, 167)

    # Filter by typing, then Escape clears it.
    app.key("f", "command")
    app.type("node")
    app.shot(out("m06-filter-node"))
    app.key("escape")
    app.shot(out("m07-filter-cleared"))

    # Collapse/expand all.
    app.key("e", "command", "shift")
    app.shot(out("m08-collapsed"))
    app.key("e", "command", "shift")

    # Keyboard: focus, toggle, drawer, copy path.
    app.key("down")
    app.key("space")
    app.shot(out("m09-one-selected"))
    app.key("return")
    app.shot(out("m10-drawer"))
    before = clipboard()
    app.click(545, 260)  # Copy
    after = clipboard()
    check("drawer Copy puts the path on the clipboard", after != before and after.startswith("/"), after[:80])
    app.shot(out("m11-copied-toast"))
    app.key("down", "option")
    app.shot(out("m12-drawer-next"))
    app.key("escape")
    app.key("escape")

    # Select all safe, review, clean.
    app.key("a", "command", "option")
    app.shot(out("m13-safe-selected"))
    app.key("return", "command")
    app.shot(out("m14-confirm"))
    target = find_color(out("m14-confirm"), DANGER[theme])
    check("confirm dialog shows the Clean button", target is not None)
    if target:
        app.click(*target)
        time.sleep(4)
    app.shot(out("m15-result-card"))

    # History, run detail.
    app.key("3", "command")
    app.shot(out("m16-history"))
    app.click(300, 338)
    app.shot(out("m17-run-detail"))
    app.key("escape")

    # Agents: guard check.
    app.key("2", "command")
    time.sleep(1)
    app.shot(out("m18-agents"))
    app.click(140, 272)
    time.sleep(2)
    app.shot(out("m19-agents-checked"))

    # Every settings section.
    app.key("4", "command")
    for name, x in SETTINGS_TABS.items():
        app.click(x, TAB_Y)
        app.shot(out(f"m20-settings-{name.lower()}"))

    # Minimum window size.
    app.resize(720, 560)
    for keys, name in [("1", "results"), ("2", "agents"), ("4", "settings")]:
        app.key(keys, "command")
        app.shot(out(f"m21-min-{name}"))
    app.resize(900, 700)
    app.key("1", "command")
    app.key("d", "command")
    app.shot(out("m22-compact"))
    return results


def persistence(app, out):
    """Settings survive a relaunch; a corrupt settings file is reported once."""
    app.key("d", "command")  # to compact
    time.sleep(1.5)  # past the save debounce
    app.quit()
    app.launch()
    app.key("r", "command")
    time.sleep(6)
    app.shot(out("p01-relaunched-compact"))
    app.quit()
    cfg = os.path.join(app.home, ".void-sandbox-config", "config.json")
    with open(cfg, "w") as f:
        f.write("{ broken")
    app.launch()
    app.shot(out("p02-corrupt-config-toast"))
    app.quit()
    app.launch()
    app.shot(out("p03-no-toast-second-time"))


SCENARIOS = {"screens": screens, "matrix": matrix, "persistence": persistence}


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
                fn = SCENARIOS[name]
                path = lambda step: os.path.join(out_dir, f"{name}-{theme}-{density}-{step}.png")
                if name == "matrix":
                    fn(app, path, theme)
                else:
                    fn(app, path)
            finally:
                app.quit()
                shutil.rmtree(home, ignore_errors=True)


if __name__ == "__main__":
    main()
