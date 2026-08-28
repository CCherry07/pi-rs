#!/usr/bin/env bash

set -euo pipefail

pi_core_script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
pi_core_workspace=$(dirname -- "$pi_core_script_dir")

cd "$pi_core_workspace"
exec cargo test --locked \
  -p pi-core \
  -p pi-agent \
  -p pi-runtime \
  -p pi-provider \
  -p pi-prompt \
  -p pi-resources \
  -p pi-session \
  -p pi-telemetry \
  -p pi-tool-support \
  -p pi-shell \
  -p pi-plugin-prompts \
  -p pi-plugin-skills \
  -p pi-plugin-read \
  -p pi-plugin-write \
  -p pi-plugin-edit \
  -p pi-plugin-hashline-edit \
  -p pi-plugin-bash \
  -p pi-plugin-grep \
  -p pi-plugin-find \
  -p pi-plugin-ls \
  "$@"
