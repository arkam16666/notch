#!/bin/sh
# X11 + compositor (picom): screen-edge glow and see-through island.
Xvfb :99 -screen 0 1440x900x24 >/dev/null 2>&1 &
sleep 1
export DISPLAY=:99
openbox >/dev/null 2>&1 &
sleep 1
hsetroot -solid "#2b2f3a" >/dev/null
picom --backend xrender >/tmp/picom.log 2>&1 &
sleep 1
ffmpeg -loglevel error -f lavfi -i "sine=frequency=330:duration=240" -metadata title="Midnight Drive" -metadata artist="The Placeholders" -y /tmp/song.ogg
mpv --no-video --ao=null /tmp/song.ogg >/dev/null 2>&1 &
sleep 1.5
export HOME=/src/linux/target/test/cfg-screen
NOTCH_TEST_CLICKS="360,16@2" GDK_BACKEND=x11 /src/linux/target/release/notch >/tmp/notch.log 2>&1 &
sleep 4
ffmpeg -loglevel error -f x11grab -video_size 1440x900 -i :99 -frames:v 1 -y /out/x11-glow.png
grep -v -E "a11y|claude usage" /tmp/notch.log | head -3
