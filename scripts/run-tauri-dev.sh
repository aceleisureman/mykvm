#!/usr/bin/env sh
set -eu

export PATH="$HOME/.cargo/bin:$PATH"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$HOME/Library/Caches/mykvm/cargo-target}"

mkdir -p "$CARGO_TARGET_DIR"

printf "Starting mykvm Tauri dev environment...\n"
printf "CARGO_TARGET_DIR=%s\n" "$CARGO_TARGET_DIR"

npm run tauri:dev
