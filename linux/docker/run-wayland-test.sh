#!/bin/sh
# Inside the test container: headless sway + mpv (MPRIS) + notch, driven by
# timed clicks; screenshots with grim into /out.
export XDG_RUNTIME_DIR=/tmp/xdg; mkdir -p $XDG_RUNTIME_DIR; chmod 700 $XDG_RUNTIME_DIR
export WLR_BACKENDS=headless WLR_LIBINPUT_NO_DEVICES=1 WLR_RENDERER=pixman
printf 'output * resolution 1440x900 bg #2b2f3a solid_color\n' > /tmp/sway.conf
sway -c /tmp/sway.conf >/tmp/sway.log 2>&1 &
sleep 2
export WAYLAND_DISPLAY=wayland-1
ffmpeg -loglevel error -f lavfi -i "sine=frequency=330:duration=240" -metadata title="Midnight Drive" -metadata artist="The Placeholders" -y /tmp/song.ogg
mpv --no-video --ao=null /tmp/song.ogg >/tmp/mpv.log 2>&1 &
sleep 2
echo "mpris: $(playerctl metadata --format '{{title}} - {{artist}} ({{status}})')"
HOME=/tmp/home; mkdir -p $HOME; export HOME
# window is 720 wide, centred on a 1440 screen → screen x − 360
#  t=3  click island (expand)      t=6  click ⏸ (428,158)
#  t=8  Claude tab (92,18)         t=10 Timer tab (126,18)   t=11 25-min preset
NOTCH_TEST_CLICKS="360,16@3;428,158@6;92,18@8;126,18@10;556,70@11" /src/linux/target/release/notch >/tmp/notch.log 2>&1 &
sleep 2.5; grim /out/1-collapsed.png
sleep 2;   grim /out/2-expanded.png;  echo "before: $(playerctl status)"
sleep 2;   echo "after pause click: $(playerctl status)"; grim /out/3-paused.png
sleep 2;   grim /out/4-claude.png
sleep 3.5; grim /out/5-timer.png
grep -v a11y /tmp/notch.log | head -5
