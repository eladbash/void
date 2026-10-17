#!/bin/sh
set -e

# Void — cleans up after your AI agents, and your agents can call it themselves
# Install: curl -fsSL https://raw.githubusercontent.com/eladbash/void/main/install.sh | sh

REPO="eladbash/void"
APP_NAME="Void.app"
INSTALL_DIR="/Applications"

# ── Checks ──────────────────────────────────
case "$(uname -s)" in
  Darwin) ;;
  *) echo "Error: Void is only supported on macOS." >&2; exit 1 ;;
esac

command -v curl >/dev/null 2>&1 || { echo "Error: curl is required." >&2; exit 1; }
command -v hdiutil >/dev/null 2>&1 || { echo "Error: hdiutil is required." >&2; exit 1; }

# ── Architecture ────────────────────────────
ARCH="$(uname -m)"
case "$ARCH" in
  arm64)  ASSET_PATTERN="aarch64.dmg" ;;
  x86_64) ASSET_PATTERN="x64.dmg" ;;
  *)      echo "Error: Unsupported architecture: $ARCH" >&2; exit 1 ;;
esac

# ── Fetch latest release ────────────────────
echo "Fetching latest release..."
RELEASE_URL="https://api.github.com/repos/${REPO}/releases/latest"
RELEASE_JSON="$(curl -fsSL "$RELEASE_URL")"

DMG_URL="$(echo "$RELEASE_JSON" | grep -o "\"browser_download_url\": *\"[^\"]*${ASSET_PATTERN}\"" | head -1 | cut -d'"' -f4)"

if [ -z "$DMG_URL" ]; then
  echo "Error: Could not find a .dmg asset for $ARCH in the latest release." >&2
  exit 1
fi

VERSION="$(echo "$RELEASE_JSON" | grep -o '"tag_name": *"[^"]*"' | head -1 | cut -d'"' -f4)"
echo "Installing Void ${VERSION} for ${ARCH}..."

# ── Download ────────────────────────────────
TMP_DIR="$(mktemp -d)"
DMG_PATH="${TMP_DIR}/Void.dmg"
trap 'rm -rf "$TMP_DIR"' EXIT

curl -fSL --progress-bar -o "$DMG_PATH" "$DMG_URL"

# ── Mount & copy ────────────────────────────
MOUNT_POINT="$(hdiutil attach -nobrowse -noautoopen "$DMG_PATH" 2>/dev/null | tail -1 | awk '{print $NF}')"

if [ ! -d "${MOUNT_POINT}/${APP_NAME}" ]; then
  APP_FOUND="$(find "$MOUNT_POINT" -maxdepth 2 -name "$APP_NAME" -type d | head -1)"
  if [ -z "$APP_FOUND" ]; then
    hdiutil detach "$MOUNT_POINT" -quiet 2>/dev/null || true
    echo "Error: Could not find ${APP_NAME} in the DMG." >&2
    exit 1
  fi
  MOUNT_APP="$APP_FOUND"
else
  MOUNT_APP="${MOUNT_POINT}/${APP_NAME}"
fi

if [ -d "${INSTALL_DIR}/${APP_NAME}" ]; then
  echo "Removing existing installation..."
  rm -rf "${INSTALL_DIR}/${APP_NAME}"
fi

echo "Copying to ${INSTALL_DIR}..."
cp -R "$MOUNT_APP" "${INSTALL_DIR}/"

# ── Cleanup ─────────────────────────────────
hdiutil detach "$MOUNT_POINT" -quiet 2>/dev/null || true

echo ""
echo "Void ${VERSION} installed to ${INSTALL_DIR}/${APP_NAME}"

# ── The void command ────────────────────────
# The app bundles the `void` CLI. Link it onto the PATH, so terminals, scripts
# and agents can run it; the link follows the app through updates. Older
# releases have no bundled CLI, and someone else's `void` is left alone.
CLI="${INSTALL_DIR}/${APP_NAME}/Contents/MacOS/void"
if [ -x "$CLI" ]; then
  LINKED=""
  for DIR in /usr/local/bin "$HOME/.local/bin"; do
    TARGET="$DIR/void"
    if [ -e "$TARGET" ] && [ ! -L "$TARGET" ]; then
      echo "Skipping $TARGET: a void that isn't Void's is already there."
      continue
    fi
    if mkdir -p "$DIR" 2>/dev/null && ln -sfn "$CLI" "$TARGET" 2>/dev/null; then
      LINKED="$TARGET"
      break
    fi
  done
  if [ -n "$LINKED" ]; then
    echo "The void command is at ${LINKED}"
    case ":$PATH:" in
      *":$(dirname "$LINKED"):"*) ;;
      *) echo "Add it to your PATH: export PATH=\"$(dirname "$LINKED"):\$PATH\"" ;;
    esac
  else
    echo "Install the void command later from Void's Agents screen."
  fi
fi

# ── Launch ──────────────────────────────────
echo "Opening Void..."
open "${INSTALL_DIR}/${APP_NAME}"
