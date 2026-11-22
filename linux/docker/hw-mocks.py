"""Populate the dbusmock NetworkManager / BlueZ mocks with a Wi-Fi card,
some networks (one connected, one saved) and paired Bluetooth devices."""
import dbus

bus = dbus.SystemBus()
MOCK = "org.freedesktop.DBus.Mock"
nm = dbus.Interface(bus.get_object("org.freedesktop.NetworkManager", "/org/freedesktop/NetworkManager"), MOCK)
dev = nm.AddWiFiDevice("wlan0", "wlan0", dbus.Int32(100))  # 100 = activated
aps = {}
for name, ssid, strength, sec in [
    ("home", "Home 5G", 90, 0x100),
    ("office", "Office", 70, 0x100),
    ("cafe", "Cafe Free WiFi", 55, 0),
    ("true", "TrueMove H", 40, 0x100),
]:
    aps[name] = nm.AddAccessPoint(dev, name, ssid, "00:11:22:33:44:%02X" % strength,
                                  dbus.UInt32(2), dbus.UInt32(2412), dbus.UInt32(54000),
                                  dbus.Byte(strength), dbus.UInt32(sec))
# (connection_name is an object-path component; ssid_name is the real name)
home = nm.AddWiFiConnection(dev, "home", "Home 5G", "wpa-psk")
nm.AddWiFiConnection(dev, "office", "Office", "wpa-psk")
nm.AddActiveConnection([dev], home, aps["home"], "home", dbus.UInt32(2))
# mark the connected network on the device itself (what nmcli's IN-USE reads)
try:
    devobj = dbus.Interface(bus.get_object("org.freedesktop.NetworkManager", dev), MOCK)
    devobj.SetProperty(dev, "org.freedesktop.NetworkManager.Device.Wireless", "ActiveAccessPoint", dbus.ObjectPath(aps["home"]))
except dbus.exceptions.DBusException:
    pass  # older dbusmock: no ActiveAccessPoint

bz = dbus.Interface(bus.get_object("org.bluez", "/"), "org.bluez.Mock")
bz.AddAdapter("hci0", "notch-test")
for mac, name in [("AA:BB:CC:00:00:01", "WH-1000XM5"), ("AA:BB:CC:00:00:02", "MX Master 3S"), ("AA:BB:CC:00:00:03", "JBL Flip 6")]:
    bz.AddDevice("hci0", mac, name)
    bz.PairDevice("hci0", mac)
print("mocks ready:", dev)
