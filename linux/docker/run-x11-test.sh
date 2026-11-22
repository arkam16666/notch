#!/bin/sh
# X11 path: Xvfb + openbox (a regular window manager) + mpv + notch.
Xvfb :99 -screen 0 1440x900x24 >/dev/null 2>&1 &
sleep 1
export DISPLAY=:99
openbox >/dev/null 2>&1 &
sleep 1
xsetroot -solid "#2b2f3a"; echo "xsetroot $?"
ffmpeg -loglevel error -f lavfi -i "sine=frequency=330:duration=240" -metadata title="Midnight Drive" -metadata artist="The Placeholders" -y /tmp/song.ogg
mpv --no-video --ao=null /tmp/song.ogg >/dev/null 2>&1 &
sleep 2
HOME=/tmp/home; mkdir -p $HOME; export HOME
NOTCH_TEST_CLICKS="360,16@3" GDK_BACKEND=x11 /src/linux/target/release/notch >/tmp/notch.log 2>&1 &
sleep 2.5
ffmpeg -loglevel error -f x11grab -video_size 1440x900 -i :99 -frames:v 1 -y /out/x11-collapsed.png
sleep 2
ffmpeg -loglevel error -f x11grab -video_size 1440x900 -i :99 -frames:v 1 -y /out/x11-expanded.png
grep -v a11y /tmp/notch.log | head -3
