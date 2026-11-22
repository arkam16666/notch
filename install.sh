#!/bin/sh
# Notch installer — macOS or Linux.
#
#   ./install.sh                 # asks (with your system pre-selected)
#   ./install.sh --mac | --linux # no questions
#   curl -fsSL https://raw.githubusercontent.com/arkam16666/notch/main/install.sh | sh
#
# From curl it fetches the source into ~/.local/share/notch-src first.
set -e
REPO="https://github.com/arkam16666/notch.git"

bold() { printf '\033[1m%s\033[0m\n' "$1"; }
ask() { # ask "question" → reads an answer, also when piped from curl
  printf '%s' "$1" >&2
  # stdin is the script itself under `curl | sh` → ask the terminal
  if [ -t 0 ]; then read -r a; else { read -r a </dev/tty; } 2>/dev/null || read -r a || a=""; fi
  echo "$a"
}

case "$(uname -s)" in
  Darwin) detected=mac ;;
  Linux) detected=linux ;;
  *) detected=unknown ;;
esac

choice=""
for arg in "$@"; do
  case "$arg" in
    --mac|--macos) choice=mac ;;
    --linux) choice=linux ;;
  esac
done
[ -n "$NOTCH_PLATFORM" ] && choice="$NOTCH_PLATFORM"

if [ -z "$choice" ]; then
  echo
  bold "  Notch — ตัวติดตั้ง"
  echo
  echo "    1) macOS   — Notch.app (MacBook ที่มี notch; เครื่องอื่นจะวาด notch ให้)"
  echo "    2) Linux   — Ubuntu / Debian / Fedora / Arch / openSUSE …"
  echo
  def=1; [ "$detected" = linux ] && def=2
  case "$detected" in
    mac) echo "    ตรวจพบ: macOS" ;;
    linux) echo "    ตรวจพบ: Linux ($( . /etc/os-release 2>/dev/null && echo "$PRETTY_NAME"))" ;;
  esac
  ans=$(ask "    เลือก [1/2] (Enter = $def): ")
  case "${ans:-$def}" in
    1|m|mac|macos) choice=mac ;;
    2|l|linux) choice=linux ;;
    *) echo "ยกเลิก"; exit 1 ;;
  esac
fi

if [ "$choice" != "$detected" ]; then
  echo
  echo "เวอร์ชัน $choice ต้องติดตั้งบนเครื่อง $choice — เครื่องนี้เป็น $detected"
  echo "ให้รันตัวติดตั้งนี้บนเครื่องที่จะใช้งาน"
  exit 1
fi

# --- source: this checkout, or clone it (curl | sh) ---
here="$(cd "$(dirname "$0")" 2>/dev/null && pwd)"
if [ -f "$here/linux/Cargo.toml" ] && [ -f "$here/Cargo.toml" ]; then
  SRC="$here"
else
  SRC="$HOME/.local/share/notch-src"
  command -v git >/dev/null || { echo "ต้องมี git ก่อน"; exit 1; }
  if [ -d "$SRC/.git" ]; then
    echo "==> อัปเดตซอร์สโค้ด ($SRC)"
    git -C "$SRC" pull -q --ff-only
  else
    echo "==> ดาวน์โหลดซอร์สโค้ด → $SRC"
    mkdir -p "$(dirname "$SRC")"
    git clone -q --depth 1 "$REPO" "$SRC"
  fi
fi

# --- Rust (both versions are built from source) ---
[ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"
if ! command -v cargo >/dev/null; then
  ans=$(ask "ต้องมี Rust (rustup.rs) — ติดตั้งให้เลยไหม? [Y/n] ")
  case "$ans" in n|N) echo "ติดตั้ง Rust ก่อน: https://rustup.rs"; exit 1 ;; esac
  curl -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal
  . "$HOME/.cargo/env"
fi

if [ "$choice" = mac ]; then
  exec sh "$SRC/scripts/install.sh"
else
  NOTCH_ASK_DEPS=1 exec sh "$SRC/linux/install.sh"
fi
