#!/bin/sh
# Headless GNOME Shell (Wayland, no layer-shell): does Notch come up on
# XWayland as a dock at the top centre?
export XDG_RUNTIME_DIR=/tmp/xdg; mkdir -p $XDG_RUNTIME_DIR; chmod 700 $XDG_RUNTIME_DIR
# GNOME needs logind on the system bus: a mock one is enough here
mkdir -p /run/dbus && dbus-daemon --system --fork --nopidfile 2>/dev/null
python3 -m dbusmock --system --template logind >/tmp/logind.log 2>&1 &
python3 -m dbusmock --system --template upower >/tmp/upower.log 2>&1 &
sleep 1.5
mkdir -p /tmp/.X11-unix && chmod 1777 /tmp/.X11-unix
export XDG_SESSION_TYPE=wayland XDG_CURRENT_DESKTOP=GNOME
gsettings set org.gnome.desktop.background picture-uri '' 2>/dev/null
gsettings set org.gnome.desktop.background primary-color '#2b2f3a' 2>/dev/null
gsettings set org.gnome.desktop.notifications show-banners false 2>/dev/null
gnome-shell --wayland --headless --unsafe-mode --virtual-monitor 1440x900 >/tmp/gs.log 2>&1 &
for i in $(seq 1 30); do [ -S $XDG_RUNTIME_DIR/wayland-0 ] && break; sleep 1; done
export WAYLAND_DISPLAY=wayland-0 DISPLAY=:0
echo "gnome-shell: $(ls $XDG_RUNTIME_DIR | tr '\n' ' ')"
sleep 4
# leave the overview that GNOME opens at start
gdbus call --session --dest org.gnome.Shell --object-path /org/gnome/Shell --method org.gnome.Shell.Eval 'Main.overview.hide(); "ok"' >/dev/null 2>&1
ffmpeg -loglevel error -f lavfi -i "sine=frequency=330:duration=240" -metadata title="Midnight Drive" -metadata artist="The Placeholders" -y /tmp/song.ogg
mpv --no-video --ao=null /tmp/song.ogg >/dev/null 2>&1 &
sleep 1.5
export XAUTHORITY=$(ls $XDG_RUNTIME_DIR/.mutter-Xwaylandauth.* 2>/dev/null | head -1)
export HOME=/tmp/home; mkdir -p $HOME
NOTCH_TEST_CLICKS="360,16@6" /src/linux/target/release/notch >/tmp/notch.log 2>&1 &
sleep 5
gdbus call --session --dest org.gnome.Shell.Screenshot --object-path /org/gnome/Shell/Screenshot --method org.gnome.Shell.Screenshot.Screenshot false false /out/gnome-collapsed.png >/dev/null 2>&1 || echo "screenshot failed"
sleep 3.5
gdbus call --session --dest org.gnome.Shell.Screenshot --object-path /org/gnome/Shell/Screenshot --method org.gnome.Shell.Screenshot.Screenshot false false /out/gnome-expanded.png >/dev/null 2>&1
echo "--- notch"; grep -v -E "a11y|claude usage" /tmp/notch.log | head -5
echo "--- windows"; gdbus call --session --dest org.gnome.Shell --object-path /org/gnome/Shell --method org.gnome.Shell.Eval 'global.get_window_actors().map(a => { const w = a.meta_window; const r = w.get_frame_rect(); return `${w.get_wm_class()} ${w.get_window_type()} ${r.x},${r.y} ${r.width}x${r.height} above=${w.is_above()} xwayland=${w.get_client_type()}`; }).join(" | ")' 2>&1 | head -3
grep -iE "error|critical" /tmp/gs.log | head -5
