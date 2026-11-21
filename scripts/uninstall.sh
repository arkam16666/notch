#!/bin/sh
# Remove Notch: stop it, remove the login item, the app and its Claude Code
# hooks. Pass --purge to also delete its data (~/.claude/notch: shelf list,
# Claude status files, ask-Claude folder).
set -e
PLIST="$HOME/Library/LaunchAgents/local.notch.plist"

launchctl bootout "gui/$(id -u)" "$PLIST" 2>/dev/null || true
rm -f "$PLIST"
pkill -x notch 2>/dev/null || true
rm -rf /Applications/Notch.app "$HOME/Applications/Notch.app"

python3 - <<'PY'
import json, os
path = os.path.expanduser("~/.claude/settings.json")
try:
    with open(path) as f:
        cfg = json.load(f)
except (FileNotFoundError, json.JSONDecodeError):
    raise SystemExit(0)
hooks = cfg.get("hooks", {})
for ev in list(hooks):
    groups = []
    for g in hooks[ev]:
        g["hooks"] = [h for h in g.get("hooks", [])
                      if not ("--hook" in h.get("command", "") and "notch" in h.get("command", "").lower())]
        if g["hooks"]:
            groups.append(g)
    if groups:
        hooks[ev] = groups
    else:
        del hooks[ev]
if not hooks:
    cfg.pop("hooks", None)
with open(path, "w") as f:
    json.dump(cfg, f, indent=2, ensure_ascii=False)
    f.write("\n")
print("removed Notch hooks from", path)
PY

if [ "$1" = "--purge" ]; then
  rm -rf "$HOME/.claude/notch"
  echo "removed ~/.claude/notch"
fi
echo "Notch uninstalled."
