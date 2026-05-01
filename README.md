# MyKVM

MyKVM is a cross-platform software KVM prototype for sharing one keyboard, mouse, and text clipboard between machines on the same trusted LAN.

It is built with Tauri 2, Rust, React, and TypeScript. The first release targets Windows, macOS, and Linux desktop builds.

[中文说明](./README.zh-CN.md)

## What It Does

- Runs in Server or Client mode.
- Discovers nearby peers on the LAN.
- Supports manual peer connection by host or IP.
- Detects local displays and lets you arrange multi-monitor layouts.
- Shares keyboard and mouse input over a single UDP transport port.
- Syncs text clipboard content over the same transport.
- Provides light, dark, and system theme modes.
- Includes English and Simplified Chinese UI.
- Supports tray behavior for hiding and restoring the main window.

## Current Status

MyKVM `v0.1.0` is an experimental first release. It is useful for local testing and iteration, but it is not hardened for untrusted networks.

- License: MIT
- Default transport: UDP `47833`
- Clipboard payload cap: 256 KB
- Security model: trusted LAN prototype
- Not yet included: pairing, authentication, encryption, or production transport hardening

Do not expose the transport port to public or untrusted networks.

## Protocol

MyKVM uses one configurable UDP transport port and separates traffic with lightweight protocol markers.

| Default port | Marker | Purpose |
| --- | --- | --- |
| UDP `47833` | `mykvm.discovery.v1` | LAN discovery, peer probe/reply, host info, and display metadata |
| UDP `47833` | `mykvm.input.v1` | Mouse movement, mouse buttons, scroll events, and keyboard events |
| UDP `47833` | `mykvm.clipboard.v1` | Text clipboard sync |

The port can be fixed in Settings. Auto mode prefers UDP `47833`, falls back through nearby ports, and can use a system-selected random UDP port if needed. Peers advertise their active `transportPort`, so discovered and manually added devices can connect to the right port.

## Requirements

- Node.js 22+
- Rust stable
- Platform desktop toolchain:
  - Windows: Microsoft C++ Build Tools
  - macOS: Xcode Command Line Tools
  - Linux: WebKitGTK and appindicator development packages

## Development

Install dependencies:

```bash
npm install
```

Run the web UI:

```bash
npm run dev
```

Run the Tauri desktop app:

```bash
npm run tauri:dev
```

Build without bundling installers:

```bash
npm run tauri:build
```

Build desktop bundles:

```bash
npm run tauri:bundle
```

## Platform Helpers

Windows:

```powershell
powershell -ExecutionPolicy Bypass -File .\scripts\check-dev-env.ps1
powershell -ExecutionPolicy Bypass -File .\scripts\run-tauri-dev.ps1
```

macOS and Linux:

```bash
sh scripts/check-dev-env.sh
sh scripts/run-tauri-dev.sh
```

macOS input capture and injection require Accessibility and Input Monitoring permissions in System Settings.

## Verification

Run these before opening a pull request or cutting a release:

```bash
npm run build
npm run lint
cargo check --manifest-path src-tauri/Cargo.toml
```

## Release

Git itself only stores and pushes source history. GitHub Actions does the actual packaging on GitHub-hosted runners.

The release workflow watches pushes to `main`:

- `feat:` publishes the next minor version, such as `v0.1.0` to `v0.2.0`.
- `fix:` publishes the next patch version, such as `v0.1.0` to `v0.1.1`.
- Other prefixes run normal checks but do not publish a release.
- If no release tag exists yet, the first `feat:` or `fix:` push publishes `v0.1.0`.

Example:

```bash
git commit -m "feat: initial desktop release"
git push origin main
```

The workflow creates the git tag, builds macOS, Windows, and Linux bundles, then publishes a GitHub Release with the generated installers.

## Project Layout

| Path | Purpose |
| --- | --- |
| `src/App.tsx` | Main React desktop console |
| `src/desktopApi.ts` | Frontend bridge to Tauri commands |
| `src/layout.ts` | Display layout transforms and adjacency logic |
| `src/runtime.ts` | Runtime status types |
| `src-tauri/src/lib.rs` | Tauri commands, discovery, clipboard, app state, and performance sampling |
| `src-tauri/src/input.rs` | Input capture, transport, and injection runtime |
| `scripts/` | Development and build helper scripts |

## Contributing

Issues and pull requests are welcome. Keep changes focused, document behavior that affects the protocol, and verify both the web build and the Tauri backend when touching shared runtime code.

See [CONTRIBUTING.md](./CONTRIBUTING.md) for commit prefixes and versioning notes.

## License

MIT. See [LICENSE](./LICENSE).
