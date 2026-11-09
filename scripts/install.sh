#!/bin/zsh
# Builds Leaf.app and installs (or updates) it in /Applications.
set -euo pipefail
cd "$(dirname "$0")/.."
./scripts/bundle.sh
pkill -f '/Applications/Leaf.app/Contents/MacOS/leaf' 2>/dev/null || true
rm -rf /Applications/Leaf.app
ditto dist/Leaf.app /Applications/Leaf.app
LSREG=/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister
$LSREG -u dist/Leaf.app 2>/dev/null || true     # Finder should only know the installed copy
$LSREG -f /Applications/Leaf.app
echo "Installed /Applications/Leaf.app"
# The `leaf` command: /usr/local/bin if writable, else ~/.local/bin (if it exists).
for dir in /usr/local/bin "$HOME/.local/bin"; do
  if [[ -d $dir && -w $dir ]]; then
    ln -sf /Applications/Leaf.app/Contents/Resources/leaf "$dir/leaf"
    echo "Installed $dir/leaf"
    break
  fi
done
