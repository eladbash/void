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

# Tests, then a sandboxed launch: seed a fake home, start the app, wait for
# its window, save a screenshot.
CMD set -e; \
    cargo test --workspace --locked 2>&1 | tail -40; \
    ./target/debug/void dev seed /tmp/home >/dev/null; \
    Xvfb :99 -screen 0 1280x800x24 & sleep 2; \
    export DISPLAY=:99 LIBGL_ALWAYS_SOFTWARE=1; \
    (VOID_HOME=/tmp/home ./target/debug/void-app > /out/app.log 2>&1 &); \
    sleep 12; \
    import -window root /out/linux-results-empty.png; \
    echo "screenshot saved"; tail -20 /out/app.log
