#!/bin/sh
set -eu

usage() {
  cat <<'EOF'
Usage: ./scripts/install-package.sh [ARCHIVE]

Install a packaged pi binary on macOS or Linux. If ARCHIVE is omitted, the
newest matching archive in dist/release/ is used.

Environment:
  INSTALL_DIR   Destination directory (default: ~/.local/bin)
EOF
}

case "${1:-}" in
  -h|--help)
    usage
    exit 0
    ;;
  '') ;;
  -*)
    echo "error: unknown option: $1" >&2
    usage >&2
    exit 2
    ;;
esac

if [ "$#" -gt 1 ]; then
  usage >&2
  exit 2
fi

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
INSTALL_DIR=${INSTALL_DIR:-"$HOME/.local/bin"}

case "$(uname -s):$(uname -m)" in
  Darwin:arm64) TARGET=aarch64-apple-darwin ;;
  Darwin:x86_64) TARGET=x86_64-apple-darwin ;;
  Linux:aarch64|Linux:arm64) TARGET=aarch64-unknown-linux-gnu ;;
  Linux:x86_64) TARGET=x86_64-unknown-linux-gnu ;;
  *)
    echo "error: unsupported installer host: $(uname -s) $(uname -m)" >&2
    exit 1
    ;;
esac

if [ "$#" -eq 1 ]; then
  case "$1" in
    /*) ARCHIVE=$1 ;;
    *) ARCHIVE=$(CDPATH= cd -- "$(dirname -- "$1")" && pwd)/$(basename -- "$1") ;;
  esac
else
  ARCHIVE=$(ls -t "$ROOT"/dist/release/pi-*-${TARGET}.tar.gz 2>/dev/null | head -n 1 || true)
  if [ -z "$ARCHIVE" ]; then
    echo "error: no ${TARGET} package found in $ROOT/dist/release" >&2
    echo "run ./scripts/package-target.sh $TARGET first, or pass an archive path" >&2
    exit 1
  fi
fi

if [ ! -f "$ARCHIVE" ]; then
  echo "error: package not found: $ARCHIVE" >&2
  exit 1
fi

CHECKSUM="$ARCHIVE.sha256"
if [ -f "$CHECKSUM" ]; then
  if command -v sha256sum >/dev/null 2>&1; then
    (cd "$(dirname -- "$ARCHIVE")" && sha256sum -c "$(basename -- "$CHECKSUM")")
  elif command -v shasum >/dev/null 2>&1; then
    (cd "$(dirname -- "$ARCHIVE")" && shasum -a 256 -c "$(basename -- "$CHECKSUM")")
  else
    echo "error: sha256sum or shasum is required to verify $CHECKSUM" >&2
    exit 1
  fi
else
  echo "warning: checksum file not found; installing without verification" >&2
fi

PACKAGE=$(basename -- "$ARCHIVE" .tar.gz)
case "$PACKAGE" in
  pi-*-${TARGET}) ;;
  *)
    echo "error: unsupported package name: $(basename -- "$ARCHIVE")" >&2
    exit 1
    ;;
esac

TMP_DIR=$(mktemp -d "${TMPDIR:-/tmp}/pi-install.XXXXXX")
INSTALL_TMP=
cleanup() {
  rm -rf "$TMP_DIR"
  if [ -n "$INSTALL_TMP" ]; then
    rm -f "$INSTALL_TMP"
  fi
}
trap cleanup EXIT HUP INT TERM

tar -xzf "$ARCHIVE" -C "$TMP_DIR"
BINARY="$TMP_DIR/$PACKAGE/pi"
if [ ! -f "$BINARY" ]; then
  echo "error: package does not contain $PACKAGE/pi" >&2
  exit 1
fi
case "$TARGET" in
  aarch64-*) EXPECTED_ARCH='arm64|aarch64' ;;
  x86_64-*) EXPECTED_ARCH='x86_64|x86-64' ;;
esac
if ! file "$BINARY" | grep -Eiq "$EXPECTED_ARCH"; then
  echo "error: packaged binary architecture does not match $TARGET" >&2
  exit 1
fi

mkdir -p "$INSTALL_DIR"
INSTALL_TMP="$INSTALL_DIR/.pi.install.$$"
cp "$BINARY" "$INSTALL_TMP"
chmod 755 "$INSTALL_TMP"
mv -f "$INSTALL_TMP" "$INSTALL_DIR/pi"
INSTALL_TMP=

if [ "$(uname -s)" = "Darwin" ]; then
  # A downloaded, unsigned archive may carry macOS's quarantine attribute.
  xattr -d com.apple.quarantine "$INSTALL_DIR/pi" 2>/dev/null || true
fi

printf 'Installed pi to %s\n' "$INSTALL_DIR/pi"
case ":$PATH:" in
  *":$INSTALL_DIR:"*) ;;
  *) printf 'Add %s to PATH before running pi.\n' "$INSTALL_DIR" ;;
esac
