#!/bin/bash
# Universal Concierge — app launcher (the .app's CFBundleExecutable).
# Double-clicking the app runs this, which starts the bundled CLI's `gui` command:
# it boots the loopback server and opens the explorer in your browser (Brave/Opera
# if installed). Clicking the icon again just re-opens the explorer for the running
# server. There's no separate window of its own — the browser is the UI.
set -e
DIR="$(cd "$(dirname "$0")" && pwd)"
# Double-clicked apps get a minimal PATH; add the usual spots so the GUI can find
# Brave/Opera/ipfs when it needs them.
export PATH="/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin:$PATH"
exec "$DIR/concierge-plugin" gui
