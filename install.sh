#!/usr/bin/env bash
set -euo pipefail

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m'

REPO="eladbash/void"
APP_NAME="Void"

echo -e "${GREEN}Installing ${APP_NAME}...${NC}"

OS=$(uname -s | tr '[:upper:]' '[:lower:]')
ARCH=$(uname -m)

case "${OS}" in
  linux*)   PLATFORM="linux" ;;
  darwin*)  PLATFORM="macos" ;;
  *)        echo -e "${RED}Unsupported OS: ${OS}${NC}"; exit 1 ;;
esac

case "${ARCH}" in
  x86_64|amd64)  ARCH="x86_64" ;;
  arm64|aarch64) ARCH="aarch64" ;;
  *)             echo -e "${RED}Unsupported architecture: ${ARCH}${NC}"; exit 1 ;;
esac

LATEST=$(curl -fsSL "https://api.github.com/repos/${REPO}/releases/latest" | grep '"tag_name"' | sed -E 's/.*"([^"]+)".*/\1/')

if [ -z "$LATEST" ]; then
  echo -e "${RED}Failed to fetch latest release${NC}"
  exit 1
fi

echo -e "Latest version: ${YELLOW}${LATEST}${NC}"

case "${PLATFORM}-${ARCH}" in
  macos-aarch64) ASSET_PATTERN="aarch64.dmg" ;;
  macos-x86_64)  ASSET_PATTERN="x64.dmg" ;;
  linux-x86_64)  ASSET_PATTERN="amd64.deb" ;;
  linux-aarch64) ASSET_PATTERN="arm64.deb" ;;
  *)             echo -e "${RED}No binary for ${PLATFORM}-${ARCH}${NC}"; exit 1 ;;
esac

DOWNLOAD_URL=$(curl -fsSL "https://api.github.com/repos/${REPO}/releases/latest" | grep "browser_download_url" | grep "${ASSET_PATTERN}" | head -1 | sed -E 's/.*"([^"]+)".*/\1/')

if [ -z "$DOWNLOAD_URL" ]; then
  echo -e "${RED}Could not find download for your platform${NC}"
  echo -e "Visit https://github.com/${REPO}/releases/latest to download manually"
  exit 1
fi

TMPDIR=$(mktemp -d)
FILENAME=$(basename "$DOWNLOAD_URL")

echo -e "Downloading ${FILENAME}..."
curl -fsSL -o "${TMPDIR}/${FILENAME}" "$DOWNLOAD_URL"

case "${FILENAME}" in
  *.dmg)
    echo -e "${GREEN}Downloaded to ${TMPDIR}/${FILENAME}${NC}"
    echo -e "Opening installer..."
    open "${TMPDIR}/${FILENAME}"
    ;;
  *.deb)
    echo -e "Installing .deb package..."
    sudo dpkg -i "${TMPDIR}/${FILENAME}"
    echo -e "${GREEN}${APP_NAME} installed successfully!${NC}"
    ;;
  *.AppImage)
    chmod +x "${TMPDIR}/${FILENAME}"
    sudo mv "${TMPDIR}/${FILENAME}" "/usr/local/bin/${APP_NAME}"
    echo -e "${GREEN}${APP_NAME} installed to /usr/local/bin/${NC}"
    ;;
esac

rm -rf "$TMPDIR" 2>/dev/null || true
