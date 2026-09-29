# Build and test Void on Linux, then launch the real app under Xvfb with
# Mesa's software renderer and screenshot its window.
#
#   docker build -f scripts/linux-smoke.Dockerfile -t void-linux .
#   docker run --rm -v "$PWD/target/linux-smoke:/out" void-linux
#
# The image doubles as the reference for the Linux build dependencies CI
# installs.
FROM rust:1-bookworm

RUN apt-get update && apt-get install -y --no-install-recommends \
      pkg-config libxkbcommon-dev libxkbcommon-x11-dev libwayland-dev \
      libfontconfig1-dev libfreetype-dev libx11-xcb-dev libxcb1-dev libvulkan-dev \
      mesa-vulkan-drivers libgl1-mesa-dri libegl1 xvfb x11-apps imagemagick \
      dbus-x11 fonts-dejavu-core git \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /src
COPY . .
RUN cargo build --workspace --locked && cargo test --workspace --locked --no-run

# Tests, then a sandboxed launch: seed a fake home, start the app on a
# virtual X display with a session bus (the tray and notifications use D-Bus),
# wait for its window and save a screenshot.
CMD cargo test --workspace --locked --no-fail-fast 2>&1 | grep -E "^test result|FAILED|panicked" ; \
    ./target/debug/void dev seed /tmp/home >/dev/null; \
    export XDG_RUNTIME_DIR=/tmp/xdg DISPLAY=:99 LIBGL_ALWAYS_SOFTWARE=1; mkdir -p -m 700 $XDG_RUNTIME_DIR; \
    Xvfb :99 -screen 0 1280x800x24 & sleep 2; \
    eval "$(dbus-launch --sh-syntax)"; \
    (VOID_HOME=/tmp/home RUST_LOG=warn ./target/debug/void-app > /out/app.log 2>&1 &); \
    sleep 15; \
    pgrep -f target/debug/void-app >/dev/null && echo "app running" || echo "app exited"; \
    xwininfo -root -tree | grep -i void || true; \
    import -window root /out/linux-results-empty.png; \
    echo "screenshot saved"; cat /out/app.log
