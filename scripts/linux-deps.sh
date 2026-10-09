#!/bin/sh
# System libraries the desktop app needs to build on Debian/Ubuntu (GPUI's
# X11/Wayland windowing, fonts and Vulkan loader). The tray and notifications
# talk D-Bus in pure Rust and need nothing here. deepclean-core and void-cli
# need none of these.
set -e
sudo apt-get update
sudo apt-get install -y --no-install-recommends \
  pkg-config libxkbcommon-dev libxkbcommon-x11-dev libwayland-dev \
  libfontconfig1-dev libfreetype-dev libx11-xcb-dev libxcb1-dev libvulkan-dev
