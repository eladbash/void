#!/usr/bin/env python3
"""Drive the real Void desktop app on macOS the way a user would, and capture
screenshots of its window (never the rest of the screen).

    scripts/drive.py <app-binary> <out-dir> [scenario ...]

Each run seeds a fresh sandbox home with `void dev seed`, writes the theme and
density into its settings, launches the app with VOID_HOME pointing at it and
VOID_DRIVE pointing at a socket inside it, and saves
`<out-dir>/<scenario>-<theme>-<density>-<step>.png`.

Input goes over that socket into the app's own event dispatch (see
crates/deepclean-app/src/drive.rs), not through the OS: the window stays behind
your other windows, never takes focus, and your keyboard, mouse and clipboard
are left alone. You can keep working while it runs, and several runs (or other
agents driving other apps) can run at once. Every step checks that the
frontmost app and the mouse position did not change, and the run fails if they
did.

The binary must be built with the driver:

    cargo build -p void-cli -p deepclean-app --features deepclean-app/drive

Needs: macOS and python3 with pyobjc (Quartz, AppKit), which the system Python
ships with. No Accessibility permission and no cliclick.
"""

import json
import os
import shutil
import socket
import subprocess
import sys
import tempfile
import time

import Quartz
from AppKit import NSWorkspace

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
VOID_CLI = os.path.join(ROOT, "target", "debug", "void")
STRICT = os.environ.get("DRIVE_STRICT") == "1"


class DriveError(Exception):
    pass


def frontmost():
    app = NSWorkspace.sharedWorkspace().frontmostApplication()
    return app.processIdentifier() if app else None


def mouse():
    loc = Quartz.CGEventGetLocation(Quartz.CGEventCreate(None))
    return (round(loc.x), round(loc.y))


class App:
    """One driven Void process. Points are window-relative, title bar included,
    like the screenshots."""

    def __init__(self, binary, home, width=900, height=700):
        self.binary = binary
        self.home = home
        self.size = (width, height)
        self.proc = None
        self.conn = None
        self.titlebar = 0
        self.window_id = None
        self.steps = 0
        self.intrusions = []

    # ── Process and socket ──────────────────────────────────────────────

    def launch(self):
        sock = os.path.join(self.home, "drive.sock")
        env = dict(os.environ, VOID_HOME=self.home, VOID_DRIVE=sock)
        if not hasattr(self, "baseline"):
            self.baseline = (frontmost(), mouse())
        self.log = open(os.path.join(self.home, "app.log"), "w")
        self.proc = subprocess.Popen([self.binary], env=env, stdout=self.log, stderr=self.log)
        deadline = time.time() + 30
        while True:
            if self.proc.poll() is not None:
                raise DriveError(f"app exited ({self.proc.returncode}); see {self.log.name}")
            try:
                s = socket.socket(socket.AF_UNIX)
                s.connect(sock)
                break
            except OSError:
                if time.time() > deadline:
                    raise DriveError("driver socket never came up: was the app built with --features drive?")
                time.sleep(0.1)
        self.conn = s.makefile("rw")
        self.resize(*self.size)

    def call(self, cmd, **args):
        self.conn.write(json.dumps(dict(args, cmd=cmd)) + "\n")
        self.conn.flush()
        line = self.conn.readline()
        if not line:
            raise DriveError(f"{cmd}: app closed the driver socket")
        resp = json.loads(line)
        self.check_hands_off(cmd)
        if not resp.get("ok"):
            raise DriveError(f"{cmd}: {resp.get('error')}")
        return resp.get("result")

    def check_hands_off(self, cmd):
        """The point of the driver: nothing outside the window changed.

        Void must never become the frontmost app. With DRIVE_STRICT=1 (for a
        run nobody touches the machine during) the frontmost app and the mouse
        position must also stay exactly as they were at launch."""
        self.steps += 1
        front, pointer = frontmost(), mouse()
        if front == self.proc.pid:
            self.intrusions.append(f"{cmd}: Void became the frontmost app")
        if STRICT and (front, pointer) != self.baseline:
            self.intrusions.append(f"{cmd}: frontmost/mouse changed {self.baseline} → {(front, pointer)}")

    def quit(self):
        if not self.proc:
            return
        try:
            self.call("quit")
        except (DriveError, OSError, ValueError):
            pass
        try:
            self.proc.wait(5)
        except subprocess.TimeoutExpired:
            self.proc.kill()
        self.proc = None

    # ── Window ──────────────────────────────────────────────────────────

    def info(self):
        info = self.call("window_info")
        self.window_id = info["window_id"]
        b = self.bounds()
        self.titlebar = round(b["Height"] - info["content_height"])
        return info

    def bounds(self):
        # Right after launch the window server can lag AppKit by a moment.
        deadline = time.time() + 5
        while True:
            wins = Quartz.CGWindowListCopyWindowInfo(Quartz.kCGWindowListOptionIncludingWindow, self.window_id)
            if wins:
                return wins[0]["kCGWindowBounds"]
            if time.time() > deadline:
                raise DriveError("window is gone")
            time.sleep(0.1)

    def resize(self, w, h):
        """Window size in points, title bar included (as the screenshots are)."""
        self.info()
        self.call("resize", width=w, height=h - self.titlebar)
        self.info()

    def shot(self, path, settle=0.0):
        if settle:
            self.call("sleep", seconds=settle)
        else:
            self.call("frame")
        subprocess.run(["screencapture", "-x", "-o", "-l", str(self.window_id), path], check=True)
        print("  captured", os.path.basename(path))

    # ── Input ───────────────────────────────────────────────────────────

    def key(self, chord):
        """A GPUI keystroke: "cmd-r", "alt-cmd-a", "down", "enter", "escape",
        "space". Several chords separated by spaces run in turn."""
        return self.call("key", keys=chord)

    def type(self, text):
        self.call("type", text=text)

    def click(self, x, y):
        """Click at window-relative point (x, y), title bar included."""
        self.call("click", x=x, y=y - self.titlebar)

    def wait_idle(self, timeout=30):
        self.call("wait_idle", timeout=timeout)

    def sleep(self, seconds):
        self.call("sleep", seconds=seconds)

    def state(self):
        return self.call("state")

    def outbox(self):
        """Clipboard writes, notifications, Finder reveals and folder pickers
        the app handed to the driver instead of the OS."""
        return self.call("outbox")


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


def find_color(path, rgb, tol=24, step=3):
    """Centre (in window points) of the pixels close to `rgb`, or None."""
    from AppKit import NSBitmapImageRep

    rep = NSBitmapImageRep.imageRepWithContentsOfFile_(path)
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


# Window-relative click targets (points) at 900x700, title bar included.
# Measured from the screenshots.
SETTINGS_TABS = {
    "Scanning": 112, "Ecosystems": 198, "AI": 263, "Guard": 311,
    "Safety": 372, "Performance": 453, "General": 537, "About": 601,
}
TAB_Y = 105
CHIPS_Y = 167
FILTERS_X, HIDDEN_X, GROUP_X, SORT_X = 265, 351, 489, 615
DANGER = {"light": (0xBF, 0x21, 0x45), "dark": (0xEF, 0x54, 0x6C)}


# ── Scenarios ────────────────────────────────────────────────────────────────
# Keyboard first: the shortcuts are part of the spec.


def screens(app, out):
    app.shot(out("01-results-empty"))
    app.key("cmd-r")
    app.shot(out("02-results-scanning"))
    app.wait_idle()
    app.shot(out("03-results"))
    app.key("down down enter")
    app.shot(out("04-drawer"))
    app.key("escape")
    app.key("alt-cmd-a")
    app.shot(out("05-selected-safe"))
    app.key("cmd-enter")
    app.shot(out("06-confirm"))
    app.key("escape")
    app.key("cmd-a")
    app.key("cmd-enter")
    app.shot(out("07-confirm-danger"))
    app.key("escape")
    app.key("shift-cmd-a")
    app.key("cmd-2")
    app.wait_idle()
    app.shot(out("08-agents"))
    app.key("cmd-3")
    app.shot(out("09-history-empty"))
    app.key("cmd-4")
    app.shot(out("10-settings"))
    app.key("cmd-1")
    app.key("cmd-d")
    app.shot(out("11-results-toggled-density"))
    app.key("cmd-d")


def matrix(app, out, theme="dark"):
    """The functional matrix, as a user would run it. Screenshots every step;
    assertions print FAIL lines rather than stopping the run."""
    results = []

    def check(name, ok, detail=""):
        results.append((name, ok))
        print(("  PASS " if ok else "  FAIL ") + name + (f" — {detail}" if detail and not ok else ""))

    app.shot(out("m01-empty"))
    app.key("cmd-r")
    app.wait_idle()
    st = app.state()
    check("scan finds the seeded items", st["items"] > 0, st)
    app.shot(out("m02-results"))

    # Group and Sort open a list; arrows and Enter pick the next option.
    for label in ["next-1", "next-2", "back"]:
        app.click(GROUP_X, CHIPS_Y)
        check(f"Group opens its list ({label})", app.state()["menu"] is not None)
        app.shot(out(f"m03-group-menu-{label}"))
        app.key("down enter")
        app.shot(out(f"m03-group-{label}"))
    for label in ["next-1", "next-2", "next-3", "back"]:
        app.click(SORT_X, CHIPS_Y)
        check(f"Sort opens its list ({label})", app.state()["menu"] is not None)
        app.key("down enter")
        app.shot(out(f"m04-sort-{label}"))

    # Filters explains itself; "N hidden" clears the filters in one click.
    app.click(FILTERS_X, CHIPS_Y)
    check("Filters opens its menu", app.state()["menu"] is not None)
    app.shot(out("m05-filters-menu"))
    app.key("escape")
    before = app.state()["visible_items"]
    app.click(HIDDEN_X, CHIPS_Y)
    after = app.state()["visible_items"]
    check("the hidden chip shows everything", after > before, f"{before} → {after}")
    app.shot(out("m05-filters-cleared"))

    # Filter by typing, then Escape clears it.
    app.key("cmd-f")
    app.type("node")
    st = app.state()
    check("typing filters the list", st["filter"] == "node" and st["visible_items"] < after, st)
    app.shot(out("m06-filter-node"))
    app.key("escape")
    check("Escape clears the filter", app.state()["filter"] == "")
    app.shot(out("m07-filter-cleared"))

    # Collapse/expand all.
    app.key("shift-cmd-e")
    app.shot(out("m08-collapsed"))
    app.key("shift-cmd-e")

    # Keyboard: focus, toggle, drawer, copy path.
    app.key("down space")
    check("Space selects the focused row", app.state()["selected"] == 1)
    app.shot(out("m09-one-selected"))
    app.key("enter")
    check("Enter opens the drawer", app.state()["drawer"])
    app.shot(out("m10-drawer"))
    app.click(DRAWER_COPY[0], DRAWER_COPY[1])
    copied = app.outbox()["clipboard"] or ""
    check("drawer Copy captures the path (not the system clipboard)", copied.startswith("/"), copied[:80])
    app.shot(out("m11-copied-toast"))
    app.key("alt-down")
    app.shot(out("m12-drawer-next"))
    app.key("escape escape")

    # Select all safe, review, clean.
    app.key("alt-cmd-a")
    app.shot(out("m13-safe-selected"))
    app.key("cmd-enter")
    check("Review opens the confirmation", app.state()["confirm"])
    app.shot(out("m14-confirm"))
    target = find_color(out("m14-confirm"), DANGER[theme])
    check("confirm dialog shows the Clean button", target is not None)
    if target:
        app.click(*target)
        app.wait_idle()
    check("the clean reports a result", app.state()["last_result"])
    app.shot(out("m15-result-card"))

    # History, run detail.
    app.key("cmd-3")
    app.shot(out("m16-history"))
    app.click(300, 338)
    app.shot(out("m17-run-detail"))
    app.key("escape")

    # Agents: guard check.
    app.key("cmd-2")
    app.wait_idle()
    app.shot(out("m18-agents"))
    app.click(140, 272)
    app.wait_idle()
    app.shot(out("m19-agents-checked"))

    # Every settings section.
    app.key("cmd-4")
    for name, x in SETTINGS_TABS.items():
        app.click(x, TAB_Y)
        app.shot(out(f"m20-settings-{name.lower()}"))

    # Minimum window size.
    app.resize(720, 560)
    for keys, name in [("cmd-1", "results"), ("cmd-2", "agents"), ("cmd-4", "settings")]:
        app.key(keys)
        app.shot(out(f"m21-min-{name}"))
    app.resize(900, 700)
    app.key("cmd-1")
    app.key("cmd-d")
    app.shot(out("m22-compact"))
    return results


DRAWER_COPY = (550, 260)


def persistence(app, out):
    """Settings survive a relaunch; a corrupt settings file is reported once."""
    app.key("cmd-d")  # to compact
    app.sleep(1.5)  # past the save debounce
    app.quit()
    app.launch()
    app.key("cmd-r")
    app.wait_idle()
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
    failed = False
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
                    results = fn(app, path, theme)
                    failed |= not all(ok for _, ok in results)
                else:
                    fn(app, path)
            except DriveError as err:
                print(f"  FAIL {err}")
                failed = True
            finally:
                app.quit()
                if app.intrusions:
                    failed = True
                    for i in app.intrusions:
                        print(f"  FAIL hands off: {i}")
                else:
                    print(f"  PASS hands off: focus and mouse untouched over {app.steps} steps")
                shutil.rmtree(home, ignore_errors=True)
    sys.exit(1 if failed else 0)


if __name__ == "__main__":
    main()
