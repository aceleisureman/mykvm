# Contributing

MyKVM uses small, focused changes and Conventional Commit style prefixes.

## Commit Prefixes

- `feat:` user-facing feature
- `fix:` bug fix
- `perf:` performance improvement
- `refactor:` internal code change without behavior change
- `ui:` visual or interaction-only change
- `docs:` documentation-only change
- `ci:` GitHub Actions or release automation
- `build:` packaging, dependency, or build-system change
- `chore:` maintenance that does not affect runtime behavior
- `test:` tests or verification changes
- `release:` version bump, changelog, and release preparation

Use a scope when it helps scanning:

```text
feat(transport): add single-port UDP fallback
fix(ui): keep header sticky while scrolling
ci(release): build Tauri bundles on tags
release: v0.1.0
```

## Versioning And Releases

Pushes to `main` can publish releases automatically:

- `feat:` creates the next minor release.
- `fix:` creates the next patch release.
- Other prefixes do not publish a release.
- If no release tag exists yet, the first `feat:` or `fix:` push creates `v0.1.0`.

Breaking protocol or config changes should be called out in the commit body and release notes.

## Before Opening A Pull Request

```bash
npm run build
npm run lint
cd src-tauri && cargo check
```
