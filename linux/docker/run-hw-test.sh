#!/bin/sh
# Wi-Fi / Bluetooth / speakers end to end: real nmcli / bluetoothctl / pactl
# against NetworkManager + BlueZ mocks and a real PulseAudio with 3 sinks.
. /src/linux/docker/hw-mocks.sh | tail -1
export XDG_RUNTIME_DIR=/tmp/xdg; mkdir -p $XDG_RUNTIME_DIR; chmod 700 $XDG_RUNTIME_DIR
pulseaudio -D --exit-idle-time=-1 >/dev/null 2>&1; sleep 1
pactl load-module module-null-sink sink_name=speakers sink_properties=device.description=Built-in-Speakers >/dev/null
pactl load-module module-null-sink sink_name=bluez_output.AA_BB_CC_00_00_01.1 sink_properties=device.description=WH-1000XM5 >/dev/null
pactl load-module module-null-sink sink_name=hdmi sink_properties=device.description=HDMI-Monitor >/dev/null
pactl set-default-sink speakers; pactl set-sink-volume speakers 60%
echo "sinks: $(pactl list short sinks | awk '{print $2}' | tr '\n' ' ') default=$(pactl get-default-sink)"
export WLR_BACKENDS=headless WLR_LIBINPUT_NO_DEVICES=1 WLR_RENDERER=pixman
printf 'output * resolution 1440x900 bg #2b2f3a solid_color\n' > /tmp/sway.conf
sway -c /tmp/sway.conf >/tmp/sway.log 2>&1 &
sleep 2
export WAYLAND_DISPLAY=wayland-1
ffmpeg -loglevel error -f lavfi -i "sine=frequency=330:duration=240" -metadata title="Midnight Drive" -metadata artist="The Placeholders" -y /tmp/song.ogg
mpv --no-video --ao=null /tmp/song.ogg >/dev/null 2>&1 &
sleep 1.5
export HOME=/tmp/home; mkdir -p $HOME
C="360,16@2;205,18@4;240,73@6;300,159@8.5;80,55@11.5;446,73@12.5;300,95@14.5;80,55@17.5;221,124@18.5;69,18@20;650,158@21;300,127@23"
NOTCH_TEST_CLICKS="$C" /src/linux/target/release/notch >/tmp/notch.log 2>&1 &
sleep 5.5;  grim /out/hw-1-controls.png
sleep 2.5;  grim /out/hw-2-wifi.png
sleep 3;    grim /out/hw-3-wifi-joined.png; echo "nmcli active: $(nmcli -t -f NAME,DEVICE connection show --active 2>/dev/null | tr '\n' ' ')"
sleep 3;    grim /out/hw-4-bt.png
sleep 3;    grim /out/hw-5-bt-connect.png; echo "bt connected: $(bluetoothctl devices Connected 2>/dev/null | tr '\n' ' ')"
sleep 1.5;  echo "volume after slider click: $(pactl get-sink-volume @DEFAULT_SINK@ | head -1 | awk -F/ '{print $2}')"
sleep 4;    grim /out/hw-6-speakers.png
sleep 2;    grim /out/hw-7-switched.png; echo "default sink after click: $(pactl get-default-sink)"
grep -v -E "a11y|claude usage" /tmp/notch.log | head -5
echo "bluez calls from notch: $(grep -c "Connect" /tmp/bluez.log) — $(grep -o "Connect[^ ]*" /tmp/bluez.log | sort | uniq -c | tr "\n" " ")"
echo "nm calls: $(grep -oE "AddAndActivateConnection|ActivateConnection" /tmp/nm.log | sort | uniq -c | tr "\n" " ")"
