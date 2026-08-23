#!/bin/sh
set -eu

usage() {
  echo "Usage: ./scripts/package-target.sh <rust-target>" >&2
}

if [ "$#" -ne 1 ]; then
  usage
  exit 2
fi

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
if [ ! -d "$ROOT/packages/pi/node_modules" ]; then
  echo "error: run npm install in $ROOT/packages/pi before packaging" >&2
  exit 1
fi

cd "$ROOT/packages/pi"
exec npm run release:dist -- --target "$1"
