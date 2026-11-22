#!/bin/sh
# Build gtk4-layer-shell as a *static* library under linux/target/, so the
# notch binary carries it inside — no distro package needed (Ubuntu 24.04,
# Mint 22, Pop!_OS 24.04 and Debian 12 don't ship one).
#
#   . ./build-layer-shell.sh      # then: cargo build --release
#
# Needs: git, meson, ninja, libwayland-dev, wayland-protocols, libgtk-4-dev.
VER="${GTK4_LAYER_SHELL_VERSION:-v1.1.1}"
HERE="$(cd "$(dirname "$0")" && pwd)"
PREFIX="$HERE/target/gtk4-layer-shell"
SRC="$HERE/target/gtk4-layer-shell-src"
if [ ! -f "$PREFIX/.built-$VER" ]; then
  rm -rf "$SRC" "$PREFIX"
  git -c advice.detachedHead=false clone -q --depth 1 --branch "$VER" https://github.com/wmww/gtk4-layer-shell "$SRC" || return 1 2>/dev/null || exit 1
  meson setup "$SRC/build" "$SRC" --prefix="$PREFIX" --libdir=lib --default-library=static \
    -Dexamples=false -Ddocs=false -Dtests=false -Dintrospection=false -Dvapi=false >/dev/null || return 1 2>/dev/null || exit 1
  ninja -C "$SRC/build" install >/dev/null || return 1 2>/dev/null || exit 1
  touch "$PREFIX/.built-$VER"
fi
export PKG_CONFIG_PATH="$PREFIX/lib/pkgconfig${PKG_CONFIG_PATH:+:$PKG_CONFIG_PATH}"
# tell gtk4-layer-shell-sys (system-deps) to link it statically
export SYSTEM_DEPS_GTK4_LAYER_SHELL_0_LINK=static
echo "gtk4-layer-shell $VER (static) → $PREFIX"
