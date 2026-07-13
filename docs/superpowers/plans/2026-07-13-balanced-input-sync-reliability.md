# Balanced Input and Sync Reliability Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Prevent bulk sync work and one failed peer from degrading real-time input while making file retries safe.

**Architecture:** Keep the existing QUIC wire packets. Add peer-scoped datagram health, bound queued reliable streams, dispatch reliable sends without blocking the transport command loop, and accept already-written duplicate file chunks.

**Tech Stack:** Rust, Tokio, Quinn, Tauri

---

### Task 1: Isolate datagram health by peer

**Files:**
- Modify: `src-tauri/src/quic_transport.rs`

- [ ] Add tests proving a failure recorded for peer A does not fast-fail peer B and success clears only the matching peer.
- [ ] Run the focused QUIC tests and verify they fail before implementation.
- [ ] Replace the global failure counter with a bounded peer-keyed health tracker.
- [ ] Ensure enqueue failure rolls back pending counters.
- [ ] Run focused QUIC tests and verify they pass.

### Task 2: Keep reliable streams from blocking input scheduling

**Files:**
- Modify: `src-tauri/src/quic_transport.rs`

- [ ] Add tests for the reliable-stream pending budget.
- [ ] Run the focused test and verify it fails before implementation.
- [ ] Add a bounded stream pending counter.
- [ ] Move stream send/retry work into spawned Tokio tasks using a shared connection registry.
- [ ] Keep connection lookup locks outside stream ACK waits.
- [ ] Run all QUIC transport tests.

### Task 3: Make file chunk retries idempotent

**Files:**
- Modify: `src-tauri/src/lib.rs`

- [ ] Add a test that submits the same valid chunk twice and verifies the file contains one copy.
- [ ] Run the focused test and verify it fails before implementation.
- [ ] Accept a duplicate of the immediately previous fully-written chunk without writing it again.
- [ ] Reject duplicates with mismatched offset or payload length.
- [ ] Run the file-transfer tests and full Rust test suite.

### Task 4: Final verification

**Files:**
- Verify: Rust backend and frontend

- [ ] Run `cargo fmt --manifest-path src-tauri/Cargo.toml -- --check`.
- [ ] Run `cargo test --manifest-path src-tauri/Cargo.toml`.
- [ ] Run `npm run lint`.
- [ ] Run `npm run build`.
- [ ] Run `git diff --check` and review the final diff.
- [ ] Commit the implementation without `claude_auto_continue.py`.
