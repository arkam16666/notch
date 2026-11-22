#!/bin/sh
# One-shot install of Notch:
#   1. build + bundle Notch.app and Notch Settings.app, copy them to
#      /Applications (or ~/Applications)
#   2. point Claude Code's hooks at the installed app (so the project folder
#      isn't needed any more)
#   3. start at login (LaunchAgent; restarts it if it crashes, but not if
#      you quit it with right-click)
#   4. launch it
# Re-run any time to update. Remove with scripts/uninstall.sh.
set -e
cd "$(dirname "$0")/.."

echo "==> building"
./scripts/bundle.sh --no-launch

DEST=/Applications
[ -w "$DEST" ] || { DEST="$HOME/Applications"; mkdir -p "$DEST"; }
APP="$DEST/Notch.app"
BIN="$APP/Contents/MacOS/notch"
PLIST="$HOME/Library/LaunchAgents/local.notch.plist"
UID_NUM=$(id -u)

echo "==> installing to $APP"
launchctl bootout "gui/$UID_NUM" "$PLIST" 2>/dev/null || true
pkill -x notch 2>/dev/null || true
sleep 0.5
rm -rf "$APP" "$DEST/Notch Settings.app"
cp -R dist/Notch.app "$APP"
cp -R "dist/Notch Settings.app" "$DEST/Notch Settings.app"

echo "==> Claude Code hooks → $BIN"
python3 - "$BIN" <<'PY'
import json, os, sys
binp = sys.argv[1]
path = os.path.expanduser("~/.claude/settings.json")
if not os.path.isdir(os.path.dirname(path)):
    print("   (Claude Code not found — skipping hooks)")
    sys.exit(0)
try:
    with open(path) as f:
        cfg = json.load(f)
except FileNotFoundError:
    cfg = {}
hooks = cfg.setdefault("hooks", {})
events = ["UserPromptSubmit", "PreToolUse", "PostToolUse", "PostToolUseFailure",
          "Notification", "Stop", "SessionEnd"]
tool_events = {"PreToolUse", "PostToolUse", "PostToolUseFailure"}
for ev in events:
    cmd = f'"{binp}" --hook {ev} 2>/dev/null || true'
    groups = hooks.setdefault(ev, [])
    found = False
    for g in groups:
        for h in g.get("hooks", []):
            if "--hook" in h.get("command", "") and "notch" in h.get("command", "").lower():
                h["command"] = cmd          # update an older Notch hook in place
                h.setdefault("timeout", 5)
                found = True
    if not found:
        g = {"hooks": [{"type": "command", "command": cmd, "timeout": 5}]}
        if ev in tool_events:
            g["matcher"] = "*"
        groups.append(g)
tmp = path + ".notch-tmp"
with open(tmp, "w") as f:
    json.dump(cfg, f, indent=2, ensure_ascii=False)
    f.write("\n")
os.replace(tmp, path)
print("   updated", path)
PY

echo "==> start at login"
rm -f "$HOME/.claude/notch/local.notch.plist.off"   # switched off in Settings → back on
mkdir -p "$(dirname "$PLIST")"
cat > "$PLIST" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>local.notch</string>
  <key>ProgramArguments</key><array><string>$BIN</string></array>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><dict><key>SuccessfulExit</key><false/></dict>
  <key>ProcessType</key><string>Interactive</string>
</dict>
</plist>
EOF
launchctl bootstrap "gui/$UID_NUM" "$PLIST"

echo "==> done — Notch is running and will start automatically at login."
