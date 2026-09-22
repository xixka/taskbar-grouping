# tbg-lite

Zero-injection taskbar grouping controller for Windows 10/11, written in Rust.

It controls taskbar button grouping by rewriting each window's
`PKEY_AppUserModel_ID` through the documented Shell property-store API
(`SHGetPropertyStoreForWindow`) — no DLL injection, no shell patching.

Two strategy lines (`watch --strategy`):

- `ungroup` (default) — per-window suffix `~TBG~w<HWND>`; enabling the watch
  ungroups everything on the taskbar, including windows that already existed
  at startup (Windhawk-mod-compatible default).
- `group --group <NAME>` — shared AUMID `TBG.Group.<NAME>` for custom
  grouping; originals are persisted to `%LOCALAPPDATA%\tbg-lite\tbg-restore.tsv`
  (atomic writes, single-instance mutex) for `restore`.

CLI: `inspect` (incl. `--json`), `set`, `watch`, `restore` (incl. `--dry-run`).
Launched with **no arguments**, tbg-lite opens an interactive menu (task 14):
start/stop the watch on either strategy line, restore all, inspect windows,
and exit through menu option `[0]` — no Ctrl+C needed; stopping the watch
from the menu is graceful (hooks removed, stats printed).

- Implementation plan and task breakdown: [`docs/plan.md`](docs/plan.md) (route B+, v2)
- Phase 0b acceptance evidence: [`docs/phase0b-acceptance.md`](docs/phase0b-acceptance.md)
- CI (windows-latest): `cargo build --release --locked` + unit tests + a
  33-assertion runtime smoke (incl. the interactive menu, driven via stdin)
  and a 30-assertion acceptance suite, both running against real windows in
  the runner session. Taskbar visuals / race perception / multi-app coverage
  still need real-machine validation.

Status: tasks 0-26 complete (Phase 0b PoC, default ungroup-on-enable,
interactive menu, audit remediation Phase R); see the task list in
[`docs/plan.md`](docs/plan.md) v2 §3.
