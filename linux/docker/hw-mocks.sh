#!/bin/sh
# System-bus mocks: NetworkManager with a Wi-Fi card + networks, BlueZ with
# an adapter + paired devices. Sourced by the hardware tests.
mkdir -p /run/dbus && dbus-daemon --system --fork --nopidfile 2>/dev/null
python3 -m dbusmock --system --template networkmanager >/tmp/nm.log 2>&1 &
python3 -m dbusmock --system --template bluez5 >/tmp/bluez.log 2>&1 &
python3 -m dbusmock --system --template logind >/tmp/logind.log 2>&1 &
sleep 2
python3 /src/linux/docker/hw-mocks.py
