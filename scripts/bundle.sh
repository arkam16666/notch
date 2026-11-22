#!/bin/sh
# Build dist/Notch.app (needed for Location permission → Wi-Fi names) and
# (re)launch it.  Usage: scripts/bundle.sh [--no-launch]
set -e
cd "$(dirname "$0")/.."
cargo build --release
APP=dist/Notch.app
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS"
cp target/release/notch "$APP/Contents/MacOS/notch"
cat > "$APP/Contents/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleIdentifier</key><string>local.notch</string>
  <key>CFBundleName</key><string>Notch</string>
  <key>CFBundleDisplayName</key><string>Notch</string>
  <key>CFBundleExecutable</key><string>notch</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>0.1</string>
  <key>CFBundleVersion</key><string>1</string>
  <key>LSMinimumSystemVersion</key><string>14.0</string>
  <key>LSUIElement</key><true/>
  <key>NSLocationUsageDescription</key><string>ใช้เพื่อแสดงชื่อเครือข่าย Wi-Fi ใน notch</string>
  <key>NSLocationWhenInUseUsageDescription</key><string>ใช้เพื่อแสดงชื่อเครือข่าย Wi-Fi ใน notch</string>
  <key>NSAudioCaptureUsageDescription</key><string>ใช้ฟังระดับเสียงเพลงที่กำลังเล่น เพื่อให้แท่ง waveform ใน notch เด้งตามเพลง (ไม่บันทึกเสียง)</string>
  <key>NSCameraUsageDescription</key><string>ใช้แสดงกล้องหน้าเป็นกระจกส่องหน้าใน notch</string>
  <key>NSBluetoothAlwaysUsageDescription</key><string>ใช้เพื่อแสดงและเชื่อมต่ออุปกรณ์ Bluetooth ใน notch</string>
  <key>NSLocalNetworkUsageDescription</key><string>ใช้ค้นหาทีวี / Apple TV ในเครือข่าย สำหรับการสะท้อนหน้าจอ</string>
  <key>NSBonjourServices</key><array><string>_airplay._tcp</string></array>
  <key>NSAppleEventsUsageDescription</key><string>ใช้ดูเพลงที่กำลังเล่นและควบคุมการเล่น</string>
</dict>
</plist>
PLIST
# Ad-hoc signature with a *stable* designated requirement (bundle id), so
# macOS privacy permissions (Accessibility, Location, Camera…) survive rebuilds
# instead of being tied to this exact binary's hash.
codesign --force --sign - -r='designated => identifier "local.notch"' "$APP" >/dev/null 2>&1
echo "built $APP"

# Separate "Notch Settings" app: same binary, started under the name
# notch-settings (→ settings window instead of the island). Regular app with
# a Dock icon, so it can be found in Launchpad / Spotlight.
SET="dist/Notch Settings.app"
rm -rf "$SET"
mkdir -p "$SET/Contents/MacOS"
cp target/release/notch "$SET/Contents/MacOS/notch-settings"
cat > "$SET/Contents/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleIdentifier</key><string>local.notch.settings</string>
  <key>CFBundleName</key><string>Notch Settings</string>
  <key>CFBundleDisplayName</key><string>Notch Settings</string>
  <key>CFBundleExecutable</key><string>notch-settings</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>0.1</string>
  <key>CFBundleVersion</key><string>1</string>
  <key>LSMinimumSystemVersion</key><string>14.0</string>
  <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
PLIST
codesign --force --sign - -r='designated => identifier "local.notch.settings"' "$SET" >/dev/null 2>&1
echo "built $SET"
if [ "$1" != "--no-launch" ]; then
  pkill -x notch 2>/dev/null || true
  sleep 0.5
  open "$APP"
fi
