#!/bin/sh
set -eu

TARGET="aarch64-apple-darwin"
ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$ROOT"

if [ "$(uname -s)" != "Darwin" ]; then
  echo "error: the macOS arm64 package must be built on macOS" >&2
  exit 1
fi

if command -v rustup >/dev/null 2>&1; then
  rustup target add "$TARGET"
fi

PACKAGE_ID=$(cargo pkgid -p pi-cli)
VERSION=${PACKAGE_ID##*#}
if [ "$VERSION" = "$PACKAGE_ID" ]; then
  VERSION=${PACKAGE_ID##*@}
fi
VERSION=${PI_VERSION:-$VERSION}
case "$VERSION" in
  *[!0-9A-Za-z.+-]*|'')
    echo "error: invalid package version: $VERSION" >&2
    exit 1
    ;;
esac

PACKAGE="pi-${VERSION}-${TARGET}"
DIST_DIR="$ROOT/dist"
STAGE_DIR="$DIST_DIR/$PACKAGE"
ARCHIVE="$DIST_DIR/$PACKAGE.tar.gz"
CHECKSUM="$ARCHIVE.sha256"
BINARY="$ROOT/target/$TARGET/release/pi"

rm -rf "$STAGE_DIR"
mkdir -p "$STAGE_DIR"

cargo build --locked --release --target "$TARGET" -p pi-cli
cp "$BINARY" "$STAGE_DIR/pi"
strip -x "$STAGE_DIR/pi"
chmod 755 "$STAGE_DIR/pi"

if ! file "$STAGE_DIR/pi" | grep -q 'arm64'; then
  echo "error: packaged binary is not Apple Silicon arm64" >&2
  exit 1
fi

cat > "$STAGE_DIR/README.txt" <<EOF
pi ${VERSION} for Apple Silicon macOS

Install:
  mkdir -p ~/.local/bin
  cp pi ~/.local/bin/pi
  chmod +x ~/.local/bin/pi

Make sure ~/.local/bin is in PATH, then configure an OpenAI-compatible API:
  export OPENAI_API_KEY="..."
  export OPENAI_MODEL="gpt-4o-mini"
  export OPENAI_BASE_URL="https://api.openai.com/v1"

Start the TUI:
  pi

If macOS blocks an unsigned downloaded binary, remove its quarantine attribute:
  xattr -d com.apple.quarantine ~/.local/bin/pi
EOF

rm -f "$ARCHIVE" "$CHECKSUM"
COPYFILE_DISABLE=1 tar -C "$DIST_DIR" -czf "$ARCHIVE" "$PACKAGE"
(
  cd "$DIST_DIR"
  shasum -a 256 "$(basename "$ARCHIVE")" > "$(basename "$CHECKSUM")"
)
rm -rf "$STAGE_DIR"

printf 'Created:\n  %s\n  %s\n' "$ARCHIVE" "$CHECKSUM"
