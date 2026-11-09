#!/bin/zsh
# Builds dist/Leaf.app.
#   PROFILE=dev     reuse the debug build (dependencies are optimized either way)
#   VERSION=1.2.3   stamp the version into Info.plist (the release workflow does this)
set -euo pipefail
cd "$(dirname "$0")/.."
PROFILE=${PROFILE:-release}
# Keep the builder's home folder and checkout path out of the binary (panic
# messages embed source paths). The last matching prefix wins.
export RUSTFLAGS="${RUSTFLAGS:-} --remap-path-prefix=$HOME=~ --remap-path-prefix=$PWD=leaf"
if [[ $PROFILE == release ]]; then cargo build --release; BIN=target/release/leaf
else cargo build; BIN=target/debug/leaf; fi

APP=dist/Leaf.app
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$BIN" "$APP/Contents/MacOS/leaf"
cp assets/Info.plist "$APP/Contents/Info.plist"
cp assets/Leaf.icns "$APP/Contents/Resources/Leaf.icns"   # regenerate: scripts/make-icon.swift + iconutil
if [[ -n ${VERSION:-} ]]; then
  plutil -replace CFBundleShortVersionString -string "$VERSION" "$APP/Contents/Info.plist"
  plutil -replace CFBundleVersion -string "$VERSION" "$APP/Contents/Info.plist"
fi
codesign --force --sign - "$APP" >/dev/null   # ad-hoc signature; not notarized
if [[ -z ${CI:-} ]]; then
  # Tell Launch Services about the app so Finder lists it under "Open With".
  /System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister -f "$APP"
fi
echo "Built $APP ($(du -sh "$APP" | cut -f1))"
