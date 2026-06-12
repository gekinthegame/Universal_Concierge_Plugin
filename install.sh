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

# ── next steps ──────────────────────────────────────────────────────────────
case ":$PATH:" in
  *":$PREFIX:"*) ;;
  *) echo
     echo "Add it to your PATH (then restart your shell):"
     echo "    export PATH=\"$PREFIX:\$PATH\"" ;;
esac
echo
echo "Done. Start the explorer with:"
echo "    $BIN gui"
