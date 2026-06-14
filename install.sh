#!/bin/sh
# Universal Concierge Plugin — one-line installer (macOS / Linux).
#
#   curl -fsSL https://github.com/gekinthegame/Universal_Concierge_Plugin/releases/latest/download/install.sh | sh
#
# Downloads the matching prebuilt `concierge-plugin` binary from the latest GitHub
# Release, verifies its checksum, and installs it to ~/.local/bin. No separate mem,
# database, or cloud; Kubo/IPFS is optional (only for publishing / the Sidekick).
set -eu

# ── the repo to install from ────────────────────────────────────────────────
REPO="${CONCIERGE_REPO:-gekinthegame/Universal_Concierge_Plugin}"
BIN="concierge-plugin"
PREFIX="${PREFIX:-$HOME/.local/bin}"

# ── detect platform ─────────────────────────────────────────────────────────
os="$(uname -s)"
arch="$(uname -m)"
case "$os" in
  Darwin) plat="macos" ;;
  Linux)  plat="linux" ;;
  *) echo "error: unsupported OS '$os' (Windows: use install.ps1)." >&2; exit 1 ;;
esac
case "$arch" in
  arm64|aarch64) a="arm64" ;;
  x86_64|amd64)  a="x64" ;;
  *) echo "error: unsupported architecture '$arch'." >&2; exit 1 ;;
esac
# Only linux-x64 is prebuilt in the first cut.
if [ "$plat" = "linux" ] && [ "$a" != "x64" ]; then
  echo "error: only linux-x64 is prebuilt right now (yours: $arch)." >&2
  exit 1
fi
asset="concierge-plugin-${plat}-${a}"
base="https://github.com/$REPO/releases/latest/download"

# ── recommend a wallet browser first (Decision 0033) ────────────────────────
# The Concierge runs best inside a Chromium wallet browser — Brave (fuller: native
# IPFS) or Opera — which power the built-in wallet, IPFS browsing, and bookmark
# memory. Strongly recommended, never required; the user picks which to install.
have_brave() {
  if [ "$plat" = "macos" ]; then [ -d "/Applications/Brave Browser.app" ] || [ -d "$HOME/Applications/Brave Browser.app" ]
  else command -v brave-browser >/dev/null 2>&1 || command -v brave >/dev/null 2>&1 || command -v brave-browser-stable >/dev/null 2>&1; fi
}
have_opera() {
  if [ "$plat" = "macos" ]; then [ -d "/Applications/Opera.app" ] || [ -d "$HOME/Applications/Opera.app" ]
  else command -v opera >/dev/null 2>&1; fi
}
open_url() {
  if [ "$plat" = "macos" ]; then open "$1" 2>/dev/null || true; else xdg-open "$1" 2>/dev/null || true; fi
}
if have_brave; then
  echo "✓ Brave detected — full Concierge experience (wallet, native IPFS, bookmark memory)."
elif have_opera; then
  echo "✓ Opera detected — wallet + bookmark memory (IPFS via gateway; Brave adds native IPFS)."
else
  echo
  echo "──────────────────────────────────────────────────────────────────────"
  echo "  The Concierge works best in a Chromium wallet browser (pick one):"
  echo "    1) Brave  (recommended — wallet · native ipfs:// · bookmark memory)"
  echo "              https://brave.com/download/"
  echo "    2) Opera  (built-in wallet · bookmark memory; IPFS via gateway)"
  echo "              https://www.opera.com/download/"
  echo "  Strongly recommended (not required)."
  echo "──────────────────────────────────────────────────────────────────────"
  if [ -t 0 ] && [ -t 1 ]; then
    printf "  Open a download page now? [1=Brave / 2=Opera / N=skip] "
    read -r reply
    case "$reply" in
      1) open_url "https://brave.com/download/" ;;
      2) open_url "https://www.opera.com/download/" ;;
    esac
  fi
  echo
fi

# ── download ────────────────────────────────────────────────────────────────
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
echo "Downloading $asset from $REPO …"
curl -fSL "$base/${asset}.tar.gz" -o "$tmp/${asset}.tar.gz"

# ── verify checksum (best-effort; skips if SHASUMS256.txt is absent) ─────────
if curl -fsSL "$base/SHASUMS256.txt" -o "$tmp/SHASUMS256.txt" 2>/dev/null; then
  expected="$(grep " ${asset}.tar.gz\$" "$tmp/SHASUMS256.txt" 2>/dev/null | awk '{print $1}' || true)"
  if [ -n "${expected:-}" ]; then
    if command -v sha256sum >/dev/null 2>&1; then
      actual="$(sha256sum "$tmp/${asset}.tar.gz" | awk '{print $1}')"
    else
      actual="$(shasum -a 256 "$tmp/${asset}.tar.gz" | awk '{print $1}')"
    fi
    if [ "$expected" != "$actual" ]; then
      echo "error: checksum mismatch for ${asset}.tar.gz." >&2
      exit 1
    fi
    echo "Checksum OK."
  fi
fi

# ── install ─────────────────────────────────────────────────────────────────
tar -xzf "$tmp/${asset}.tar.gz" -C "$tmp"
src="$tmp/${asset}/${BIN}"
[ -f "$src" ] || { echo "error: ${BIN} not found in the archive." >&2; exit 1; }

mkdir -p "$PREFIX"
if install -m 0755 "$src" "$PREFIX/$BIN" 2>/dev/null; then :; else
  cp "$src" "$PREFIX/$BIN" && chmod 0755 "$PREFIX/$BIN"
fi
echo "Installed → $PREFIX/$BIN"

# ── connect to Claude Code as an MCP server (best-effort) ────────────────────
echo
"$PREFIX/$BIN" setup 2>&1 || true

# ── next steps ──────────────────────────────────────────────────────────────
echo
case ":$PATH:" in
  *":$PREFIX:"*)
    echo "Done. Start it with:"
    echo "    $BIN gui"
    ;;
  *)
    # Add $PREFIX to the shell profile so the command works in new terminals.
    rc=""
    case "${SHELL:-}" in
      */zsh)  rc="$HOME/.zshrc" ;;
      */bash) rc="$HOME/.bashrc" ;;
    esac
    if [ -n "$rc" ] && ! grep -qsF "$PREFIX" "$rc" 2>/dev/null; then
      printf '\n# Added by the Universal Concierge Plugin installer\nexport PATH="%s:$PATH"\n' "$PREFIX" >> "$rc"
      echo "Added $PREFIX to your PATH in $rc (effective in new terminals)."
    fi
    echo
    echo "Start it right now in THIS terminal:"
    echo "    export PATH=\"$PREFIX:\$PATH\" && $BIN gui"
    echo
    echo "…or open a NEW terminal and just run:  $BIN gui"
    ;;
esac
echo
echo "It's a command-line tool — '$BIN gui' starts the explorer and opens it in"
echo "your browser (Brave/Opera if installed). There is no separate app icon."
