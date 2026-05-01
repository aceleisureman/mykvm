#!/usr/bin/env sh
set -eu

if [ "$(uname -s)" != "Darwin" ]; then
  printf "macOS ARM packaging must run on macOS because Tauri needs Apple's SDK and bundling tools.\n" >&2
  exit 1
fi

export PATH="$HOME/.cargo/bin:$PATH"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$HOME/Library/Caches/mykvm/cargo-target}"

mkdir -p "$CARGO_TARGET_DIR"

rustup target add aarch64-apple-darwin
npm install
npm run tauri:build:mac-arm
