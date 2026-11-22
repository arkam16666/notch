#!/bin/sh
# Build and install Notch for the current user on any distro
# (~/.local/bin, menu entry, start at login). Missing build tools are
# offered for install with your package manager.
set -e
cd "$(dirname "$0")"

ask() {
  printf '%s' "$1" >&2
  # stdin is the script itself under `curl | sh` → ask the terminal
  if [ -t 0 ]; then read -r a; else { read -r a </dev/tty; } 2>/dev/null || read -r a || a=""; fi
  echo "$a"
}

if command -v apt-get >/dev/null; then
  PM="sudo apt-get install -y"; PKGS="build-essential pkg-config git curl libgtk-4-dev libdbus-1-dev meson ninja-build libwayland-dev wayland-protocols pulseaudio-utils wl-clipboard"
elif command -v dnf >/dev/null; then
  PM="sudo dnf install -y"; PKGS="gcc pkgconf-pkg-config git curl gtk4-devel dbus-devel meson ninja-build wayland-devel wayland-protocols-devel pulseaudio-utils wl-clipboard"
elif command -v pacman >/dev/null; then
  PM="sudo pacman -S --needed --noconfirm"; PKGS="base-devel git curl gtk4 meson wayland wayland-protocols libpulse wl-clipboard"
elif command -v zypper >/dev/null; then
  PM="sudo zypper install -y"; PKGS="gcc pkg-config git curl gtk4-devel dbus-1-devel meson ninja wayland-devel wayland-protocols-devel pulseaudio-utils wl-clipboard"
else
  PM=""; PKGS=""
fi

if ! pkg-config --exists gtk4 2>/dev/null || ! command -v meson >/dev/null || ! command -v ninja >/dev/null; then
  echo "ต้องมีไลบรารีสำหรับ build ก่อน (GTK 4, meson, wayland-protocols …)"
  if [ -n "$PM" ]; then
    echo "  $PM $PKGS"
    ans=""
    [ -n "$NOTCH_ASK_DEPS" ] && ans=$(ask "ติดตั้งให้เลยไหม? [Y/n] ")
    case "$ans" in
      n|N) exit 1 ;;
      *) if [ -n "$NOTCH_ASK_DEPS" ]; then $PM $PKGS; else exit 1; fi ;;
    esac
  else
    echo "  ติดตั้ง GTK 4 (-dev / -devel), meson, ninja, wayland-protocols, git และ pkg-config"
    exit 1
  fi
fi
[ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"
command -v cargo >/dev/null || { echo "ต้องมี Rust: https://rustup.rs"; exit 1; }

echo "==> building (gtk4-layer-shell is built in)"
. ./build-layer-shell.sh
cargo build --release
echo "==> installing to ~/.local"
install -Dm755 target/release/notch "$HOME/.local/bin/notch"
install -Dm644 ../docs/icon.png "$HOME/.local/share/icons/hicolor/256x256/apps/notch.png"
mkdir -p "$HOME/.local/share/applications" "$HOME/.config/autostart"
sed "s|^Exec=notch|Exec=$HOME/.local/bin/notch|" packaging/notch.desktop > "$HOME/.local/share/applications/notch.desktop"
sed "s|^Exec=notch|Exec=$HOME/.local/bin/notch|" packaging/notch-autostart.desktop > "$HOME/.config/autostart/notch.desktop"
if [ -d "$HOME/.claude" ]; then "$HOME/.local/bin/notch" --setup-claude; fi
echo "==> starting"
pkill -x notch 2>/dev/null || true
if [ -n "$WAYLAND_DISPLAY$DISPLAY" ]; then
  nohup "$HOME/.local/bin/notch" >/dev/null 2>&1 &
  echo "done — Notch เปิดอยู่ และจะเปิดเองตอนเข้าเครื่อง"
else
  echo "done — Notch จะเปิดเองตอนเข้าเครื่องครั้งถัดไป (ตอนนี้ไม่ได้อยู่ในหน้าจอกราฟิก)"
fi
