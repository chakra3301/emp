#!/usr/bin/env bash
# Build EMP.app (UI + release empd + Tauri shell) and install it to /Applications.
#
#   npm run app                      build, install, leave a running daemon alone
#   npm run app -- --restart-daemon  also restart empd on the new build (cuts off running replies)
#
# The app installs/refreshes the login item (~/Library/LaunchAgents/com.lucaorion.emp.empd.plist)
# on launch and starts empd through launchd when it isn't running.
set -euo pipefail

cd "$(dirname "$0")/.."
ROOT="$PWD"
LABEL="com.lucaorion.emp.empd"
DEST="/Applications/EMP.app"
DATA="${EMP_DATA_DIR:-$HOME/Library/Application Support/EMP}"
restart=0
[[ "${1:-}" == "--restart-daemon" ]] && restart=1

step() { printf '\n\033[1m==> %s\033[0m\n' "$*"; }

step "UI"
npm run build -w @emp/desktop >/dev/null

step "empd (release)"
cargo build --release -p empd
triple="$(rustc -vV | sed -n 's/^host: //p')"
cp target/release/empd "apps/desktop/src-tauri/binaries/empd-$triple"

step "EMP.app"
(cd apps/desktop && npx tauri build --bundles app)
built="apps/desktop/src-tauri/target/release/bundle/macos/EMP.app"
[[ -d "$built" ]] || { echo "no bundle at $built" >&2; exit 1; }

step "Install to $DEST"
# Quit a running copy (its process is emp-desktop) so the new one actually launches next time.
was_running=0
if pgrep -f "$DEST/Contents/MacOS/emp-desktop" >/dev/null; then
  was_running=1
  osascript -e 'quit app id "com.lucaorion.emp"' >/dev/null 2>&1 || true
  for _ in 1 2 3 4 5 6 7 8 9 10; do pgrep -f "$DEST/Contents/MacOS/emp-desktop" >/dev/null || break; sleep 0.5; done
fi
rm -rf "$DEST"
ditto "$built" "$DEST"
echo "installed $(du -sh "$DEST" | cut -f1) at $DEST"
[[ $was_running == 1 ]] && open "$DEST"

uid="$(id -u)"
running_pid="$(sed -n 's/.*"pid": *\([0-9]*\).*/\1/p' "$DATA/daemon.json" 2>/dev/null || true)"
if [[ $restart == 1 ]]; then
  step "Restart empd"
  if launchctl print "gui/$uid/$LABEL" >/dev/null 2>&1; then
    # Stop a hand-started daemon first so the launchd one can take over.
    if [[ -n "$running_pid" ]] && ! launchctl print "gui/$uid/$LABEL" | grep -q "pid = $running_pid"; then
      kill "$running_pid" 2>/dev/null || true
      sleep 2
    fi
    launchctl kickstart -k "gui/$uid/$LABEL"
    echo "empd restarted under launchd"
  else
    echo "login item not loaded yet; open EMP once, then re-run with --restart-daemon"
  fi
elif [[ -n "$running_pid" ]] && kill -0 "$running_pid" 2>/dev/null; then
  echo
  echo "empd is already running (pid $running_pid); the app connects to it. The new empd takes"
  echo "over at the next login, or now with: npm run app -- --restart-daemon"
fi

echo
echo "Open EMP from /Applications, Spotlight or the Dock."
