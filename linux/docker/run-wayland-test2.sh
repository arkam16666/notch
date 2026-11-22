#!/bin/sh
# Phase-2 checks on headless sway: controls tab, speaker list, settings
# window, screen-edge glow.
export XDG_RUNTIME_DIR=/tmp/xdg; mkdir -p $XDG_RUNTIME_DIR; chmod 700 $XDG_RUNTIME_DIR
export WLR_BACKENDS=headless WLR_LIBINPUT_NO_DEVICES=1 WLR_RENDERER=pixman
printf 'output * resolution 1440x900 bg #2b2f3a solid_color\n' > /tmp/sway.conf
sway -c /tmp/sway.conf >/tmp/sway.log 2>&1 &
sleep 2
export WAYLAND_DISPLAY=wayland-1
ffmpeg -loglevel error -f lavfi -i "sine=frequency=330:duration=240" -metadata title="Midnight Drive" -metadata artist="The Placeholders" -y /tmp/song.ogg
mpv --no-video --ao=null /tmp/song.ogg >/dev/null 2>&1 &
sleep 1.5
export HOME=/src/linux/target/test/cfg-screen
# expand, then the controls tab (5th pill: x = 40+14+4*34+15 = 205)
NOTCH_TEST_CLICKS="360,16@2;205,18@4" /src/linux/target/release/notch >/tmp/notch.log 2>&1 &
sleep 3.5; grim /out/p2-screen-glow.png
sleep 2;   grim /out/p2-controls.png
# settings window from a second launch (forwarded to the running app)
/src/linux/target/release/notch --settings >/dev/null 2>&1 &
sleep 2.5; grim /out/p2-settings.png
grep -v -E "a11y|claude usage" /tmp/notch.log | head -5
