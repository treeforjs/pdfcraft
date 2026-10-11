#!/usr/bin/env bash
# Install Ubuntu build or runtime dependencies. Run with sudo; this never installs Rust.
# Usage: sudo packaging/linux/install-ubuntu-deps.sh [build|runtime|smoke]
# Targets Ubuntu 24.04 / 26.04 LTS, x86_64; package names also work on release CI's 22.04.
set -euo pipefail

case "${1:-build}" in
  build)
    packages=(build-essential cmake curl ca-certificates pkg-config
      libxkbcommon-dev libwayland-dev libx11-dev libxrandr-dev libxi-dev libgl1-mesa-dev)
    ;;
  runtime)
    packages=(libxkbcommon0 libxkbcommon-x11-0 libwayland-client0 libwayland-cursor0
      libwayland-egl1 libx11-6 libx11-xcb1 libxcursor1 libxi6 libxrandr2 libgl1 libegl1
      libvulkan1 libgl1-mesa-dri mesa-vulkan-drivers xdg-desktop-portal xdg-desktop-portal-gtk cups-client)
    ;;
  smoke)
    packages=(xvfb xauth dbus-x11 jq libgl1-mesa-dri mesa-vulkan-drivers)
    ;;
  *) echo "usage: $0 [build|runtime|smoke]" >&2; exit 2 ;;
esac
if (( $# > 1 )); then
  echo "usage: $0 [build|runtime|smoke]" >&2
  exit 2
fi
if (( EUID != 0 )); then
  echo "Run this script with sudo to install Ubuntu packages." >&2
  exit 1
fi
apt-get update
apt-get install -y --no-install-recommends "${packages[@]}"
