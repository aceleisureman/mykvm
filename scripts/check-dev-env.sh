#!/usr/bin/env sh
set -u

failures=0

section() {
  printf "\n== %s ==\n" "$1"
}

check() {
  label=$1
  shift

  printf "[check] %s\n" "$label"
  if "$@"; then
    :
  else
    failures=$((failures + 1))
    printf "  missing or failed: %s\n" "$label"
  fi
}

export PATH="$HOME/.cargo/bin:$PATH"

section "Versions"
check "Node.js" node --version
check "npm" npm --version
check "rustc" rustc --version
check "cargo" cargo --version
check "local Tauri CLI" npm exec tauri -- --version

section "macOS desktop prerequisites"
check "macOS version" sw_vers -productVersion
check "Xcode command line tools" xcode-select -p
check "clang" xcrun clang --version

section "Hint"
printf "If Xcode tools are missing, run: xcode-select --install\n"
printf "Then run: npm install && npm run tauri:dev\n"
printf "Native input capture/injection is still stubbed. When enabled, macOS will also need Accessibility/Input Monitoring permissions.\n"

if [ "$failures" -gt 0 ]; then
  printf "\nEnvironment is not ready: %s check(s) failed.\n" "$failures"
  exit 1
fi

printf "\nEnvironment looks ready for the current mykvm desktop prototype.\n"
