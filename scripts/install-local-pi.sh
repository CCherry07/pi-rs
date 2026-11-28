#!/bin/sh
set -eu

usage() {
  cat <<'EOF'
Usage: ./scripts/install-local-pi.sh [--release]

Build the Node launcher and native binding from this checkout, then install
pi-rs and pi launchers that use those exact local artifacts.

Options:
  --release   Build an optimized native binding instead of the default debug build.

Environment:
  INSTALL_DIR Destination directory for pi and pi-rs (default: ~/.local/bin).
EOF
}

PROFILE=debug
case "${1:-}" in
  '') ;;
  --release) PROFILE=release ;;
  -h|--help)
    usage
    exit 0
    ;;
  *)
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
PACKAGE_DIR="$ROOT/packages/pi"
INSTALL_DIR=${INSTALL_DIR:-"$HOME/.local/bin"}

if [ ! -d "$PACKAGE_DIR/node_modules" ]; then
  echo "error: JavaScript dependencies are missing" >&2
  echo "run: npm install --prefix $PACKAGE_DIR" >&2
  exit 1
fi

case "$(uname -s):$(uname -m)" in
  Darwin:arm64) NAPI_SUFFIX=darwin-arm64 ;;
  Darwin:x86_64) NAPI_SUFFIX=darwin-x64 ;;
  Linux:aarch64|Linux:arm64) NAPI_SUFFIX=linux-arm64-gnu ;;
  Linux:x86_64) NAPI_SUFFIX=linux-x64-gnu ;;
  *)
    echo "error: unsupported local host: $(uname -s) $(uname -m)" >&2
    exit 1
    ;;
esac

(
  cd "$PACKAGE_DIR"
  npm run build
  if [ "$PROFILE" = release ]; then
    npm run build:native:release
  else
    npm run build:native
  fi
)

NODE_ENTRY="$PACKAGE_DIR/dist/bin/pi.js"
NATIVE_BINDING="$PACKAGE_DIR/pi-napi.$NAPI_SUFFIX.node"
if [ ! -f "$NODE_ENTRY" ]; then
  echo "error: Node launcher build did not produce $NODE_ENTRY" >&2
  exit 1
fi
if [ ! -f "$NATIVE_BINDING" ]; then
  echo "error: native build did not produce $NATIVE_BINDING" >&2
  exit 1
fi

VERSION=$(cd "$PACKAGE_DIR" && node -p "require('./package.json').version")
mkdir -p "$INSTALL_DIR"

PI_RS_LAUNCHER="$INSTALL_DIR/pi-rs"
PI_LAUNCHER="$INSTALL_DIR/pi"
if [ -d "$PI_RS_LAUNCHER" ] && [ ! -L "$PI_RS_LAUNCHER" ]; then
  echo "error: launcher destination is a directory: $PI_RS_LAUNCHER" >&2
  exit 1
fi
if [ -d "$PI_LAUNCHER" ] && [ ! -L "$PI_LAUNCHER" ]; then
  echo "error: launcher destination is a directory: $PI_LAUNCHER" >&2
  exit 1
fi

shell_quote() {
  printf "'"
  printf '%s' "$1" | sed "s/'/'\\\\''/g"
  printf "'"
}

NODE_ENTRY_QUOTED=$(shell_quote "$NODE_ENTRY")
NATIVE_BINDING_QUOTED=$(shell_quote "$NATIVE_BINDING")
VERSION_QUOTED=$(shell_quote "$VERSION")
LAUNCHER_TMP=$(mktemp "$INSTALL_DIR/.pi-rs.XXXXXX")
LINK_TMP="$INSTALL_DIR/.pi.link.$$"

cleanup() {
  rm -f "$LAUNCHER_TMP" "$LINK_TMP"
}
trap cleanup EXIT HUP INT TERM

cat > "$LAUNCHER_TMP" <<EOF
#!/bin/sh
set -eu

pi_node_entry=$NODE_ENTRY_QUOTED
pi_native_binding=$NATIVE_BINDING_QUOTED
pi_version=$VERSION_QUOTED

case "\${1:-}" in
  --version|-V)
    printf 'pi %s\n' "\$pi_version"
    exit 0
    ;;
esac

exec env PI_RS_NATIVE_BINDING="\$pi_native_binding" node "\$pi_node_entry" "\$@"
EOF
chmod 755 "$LAUNCHER_TMP"
mv -f "$LAUNCHER_TMP" "$PI_RS_LAUNCHER"

ln -s "$PI_RS_LAUNCHER" "$LINK_TMP"
mv -f "$LINK_TMP" "$PI_LAUNCHER"
trap - EXIT HUP INT TERM

printf 'Installed local %s build:\n' "$PROFILE"
printf '  pi-rs: %s\n' "$PI_RS_LAUNCHER"
printf '  pi:    %s -> %s\n' "$PI_LAUNCHER" "$PI_RS_LAUNCHER"
printf '  NAPI:  %s\n' "$NATIVE_BINDING"
"$PI_LAUNCHER" --version

