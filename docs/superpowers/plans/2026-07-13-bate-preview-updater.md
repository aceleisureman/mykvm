# Bate Preview Updater Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Publish and update stable and `bate` preview builds from `aceleisureman/mykvm`.

**Architecture:** Keep the existing Tauri updater and GitHub Actions release pipeline. Replace hard-coded upstream repository URLs with the user's repository while preserving the stable `latest` and beta `beta/latest.json` channels.

**Tech Stack:** GitHub Actions, Tauri Updater v2, TypeScript, JSON

---

### Task 1: Point application links and updater endpoints to the owned repository

**Files:**
- Modify: `src/constants.ts`
- Modify: `src-tauri/tauri.conf.json`
- Modify: `src-tauri/src/lib.rs`

- [x] Replace `https://github.com/XxMinor/mykvm` with `https://github.com/aceleisureman/mykvm`.
- [x] Verify no runtime repository URL still references `XxMinor/mykvm` with `rg -n "XxMinor/mykvm" src src-tauri`.

### Task 2: Point beta build artifacts to the owned beta release channel

**Files:**
- Modify: `.github/workflows/release.yml`

- [x] Change the beta updater endpoint to `https://github.com/aceleisureman/mykvm/releases/download/beta/latest.json`.
- [x] Confirm the workflow still triggers for both `main` and `bate`.
- [x] Validate the YAML parses successfully.

### Task 3: Verify and commit

**Files:**
- Test: application and workflow configuration

- [x] Run `npm run build` and expect a successful Vite production build.
- [x] Run `cargo test --manifest-path src-tauri/Cargo.toml` and expect all tests to pass.
- [x] Run `git diff --check` and verify no whitespace errors.
- [ ] Commit the updater and release configuration changes.
