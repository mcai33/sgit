# Startup and Exit Sync Implementation Plan

**Goal** Pull safe updates at startup and allow normal exit only after every registered repository is synchronized.

- [x] Exercise real Git remotes with startup, safe exit pulls, explicit push approval, untracked selection, stale plans, and unavailable remotes in `src/lifecycle_tests.rs`.
- [x] Share Git preflight and fast-forward logic in `src/git.rs`. Add read-only-with-respect-to-push exit inspection and explicit approval in `src/lifecycle.rs`.
- [x] Gate native window close and application exit in `desktop/src-tauri/src/main.rs`. Expose the confirmation flow and per-repository blockers in the Svelte UI.
- [x] Run Rust tests, frontend checks, native build, and a macOS close/quit smoke test.
