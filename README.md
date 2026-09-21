# tbg-lite

Zero-injection taskbar grouping controller for Windows 10/11, written in Rust.

It controls taskbar button grouping by rewriting each window's
`PKEY_AppUserModel_ID` through the documented Shell property-store API
(`SHGetPropertyStoreForWindow`) — no DLL injection, no shell patching.

- Implementation plan and task breakdown: [`docs/plan.md`](docs/plan.md) (route B+)
- CI (windows-latest) verifies **compilation only**; runtime behavior must be
  validated on a real Windows machine.

Status: early scaffold (task 0).
