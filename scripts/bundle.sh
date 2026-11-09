#!/bin/zsh
# Builds dist/Leaf.app. PROFILE=dev reuses the debug build (deps are optimized either way).
set -euo pipefail
cd "$(dirname "$0")/.."
PROFILE=${PROFILE:-release}
if [[ $PROFILE == release ]]; then cargo build --release; BIN=target/release/leaf
else cargo build; BIN=target/debug/leaf; fi

APP=dist/Leaf.app
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$BIN" "$APP/Contents/MacOS/leaf"
cp assets/Info.plist "$APP/Contents/Info.plist"
cp assets/Leaf.icns "$APP/Contents/Resources/Leaf.icns"   # regenerate: scripts/make-icon.swift + iconutil
codesign --force --sign - "$APP" >/dev/null   # ad-hoc signature for local use
# Tell Launch Services about the app so Finder lists it under "Open With".
/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister -f "$APP"
echo "Built $APP ($(du -sh "$APP" | cut -f1))"
