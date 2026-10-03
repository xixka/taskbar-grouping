# tbg-lite

Taskbar grouping controller for Windows 11 (support scope per the
2026-10-01 maintainer decision — Windows 10 users are served by other
established tools), written in Rust. tbg-lite is **zero-injection**: it
controls taskbar button grouping by rewriting each window's
`PKEY_AppUserModel_ID` through the documented Shell property-store API
(`SHGetPropertyStoreForWindow`) — no DLL injection, no shell patching, no
admin rights. Single exe, ~1.5 MB private working set.

> **History note (2026-10-02).** A separate injection edition
> (`tbg-inject.exe` + `tbg_hook.dll`, route A) shipped between 2026-09-28
> and 2026-10-02 and was then removed (plan v2 §0-8): on Windows 11 the
> taskbar never reads the grouping AUMID through the documented
> property-store call (known limitation 5, measured on CI and on a classic
> taskbar), so the in-process hook could not change grouping — zero
> user-visible effect. Its code and technical findings stay in git history
> and old release downloads; see `docs/plan.md` §5 in git history
> (final version: commit `bfd7de9`).

The two strategy lines (`watch --strategy`):

- `ungroup` (default) — per-window suffix `~TBG~w<HWND>`; enabling the watch
  ungroups everything on the taskbar, including windows that already existed
  at startup (Windhawk-mod-compatible default).
- `group --group <NAME>` — shared AUMID `TBG.Group.<NAME>` for custom
  grouping; originals are persisted to `%LOCALAPPDATA%\tbg-lite\tbg-restore.tsv`
  (atomic writes, single-instance mutex) for `restore`.

> **Windows 11 only.** OS builds below 22000 — every Windows 10 release —
> are not supported or tested; use an established tool there instead. The
> exe prints a one-line warning to stderr when launched on an out-of-scope
> build and then runs on as before (warn-only: no blocking, no exit-code
> change). The check reads the real OS build via `RtlGetVersion`
> (`src/oscheck.rs`), so it is unaffected
> by compatibility-mode manifests.

## Installation

- **Stable**: [latest release](https://github.com/xixka/taskbar-grouping/releases/latest)
  (`v*` tags; `tbg-lite-<tag>-x86_64-windows.zip` single exe, with
  `SHA256SUMS.txt` and a build-provenance attestation).
- **Dev channel**: the rolling [`dev` prerelease](https://github.com/xixka/taskbar-grouping/releases/tag/dev) —
  rebuilt from the latest push that passed the full CI suite (see
  "Evidence & verification" below); same artifact format, prerelease
  quality bar, tag always points at the exact commit it was built from.

## CLI

```
tbg-lite                              (no arguments: interactive menu)
tbg-lite inspect [--hwnd <HEX>] [--all] [--json]
tbg-lite set --hwnd <HEX> (--suffix | --value <APPID>)
tbg-lite watch [--strategy <ungroup|group>] [--group <NAME>]
               [--duration <SECS>] [--dry-run] [--verbose] [--log]
tbg-lite restore [--hwnd <HEX>] [--dry-run]
tbg-lite install [--strategy <ungroup|group>] [--group <NAME>]
tbg-lite uninstall
tbg-lite status
tbg-lite pin --group <NAME> --target <PATH> [--icon <PATH[,INDEX]>]
             [--args <STR>] [--out <DIR> | --to-taskbar]
tbg-lite unpin --group <NAME>
```

Launched with **no arguments**, tbg-lite opens an interactive menu: start/stop
the watch on either strategy line, restore all, inspect windows, and exit
through menu option `[0]` — no Ctrl+C needed; stopping the watch from the menu
is graceful (hooks removed, stats printed).

- `install` registers a per-user HKCU Run autostart (no admin) whose command
  runs `watch` along the chosen line with `--duration 0`; `uninstall` is
  idempotent; `status` gives a read-only one-glance view (autostart command,
  marked-window counters per line, restore-map state).
- `pin` generates a taskbar tile `.lnk` carrying the group's shared AUMID
  (default `%LOCALAPPDATA%\tbg-lite\pin\<NAME>.lnk`). With `--to-taskbar` the
  tile is written straight into the per-user taskbar pinned folder and
  registered via the shell's `taskbarpin` verb, so pinned tiles and live
  windows rewritten by the group watch share the AUMID and merge into one
  taskbar button. `unpin --group <NAME>` is the reverse (it verifies the
  AUMID before deleting — a foreign shortcut with the same file name is
  refused, never touched).
- `watch --log` enables the bounded ring log
  (`%LOCALAPPDATA%\tbg-lite\tbg.log`, 256 KiB cap). The resident watch also
  monitors the shell: if explorer.exe restarts, all windows are re-swept and
  re-marked automatically; if the process exits abnormally 3 times in a row,
  the circuit breaker removes the autostart entry to prevent a boot loop.

## Removed: the injection edition (2026-10-02)

The former `tbg-inject.exe` + `tbg_hook.dll` pair (route A, shipped
2026-09-28) has been removed. Its clean-room interception was verified
working at the API level, but on Windows 11 the taskbar resolves the
grouping AUMID through internal WinRT/CTaskBand paths and never through
the documented property-store call (known limitation 5, measured on CI
2026-09-30 and confirmed structural on a classic taskbar) — so grouping
never changed while it was active. With the maintainer's goal ("the
Windhawk mod's effect without Windhawk's memory cost") fully served by
tbg-lite (user-app coverage 7/7, 0% race, ~1.5 MB working set), the
edition was deleted rather than shipped as a no-effect canary
(decision log: `docs/plan.md` §0-8 in git history, commit `bfd7de9`;
technical findings: its §5).

Practical notes for past users:

- **Nothing to uninstall** — the injection edition never registered an
  autostart and never wrote restore-table entries.
- If a pre-2026-10-01 build left `tbg_hook.dll` resident in explorer
  (the round-9 bug: `stop: ok` but the file stayed locked), clear it
  once: restart explorer (`taskkill /f /im explorer.exe` then
  `start explorer`) or reboot.
- Old zips remain downloadable from past releases for reference only.

## Coexistence with Windhawk

tbg-lite never injects into explorer, so it can run alongside Windhawk and its
mods. Two things to keep in mind:

- The `taskbar-grouping` Windhawk mod implements the same feature via symbol
  hooks **inside** explorer; running both simultaneously would fight over the
  same windows' AUMIDs. Pick one — if you install the Windhawk mod, stop the
  tbg-lite watch (`menu [3]` or simply don't autostart it) and `restore`.
- Explorer folder windows self-manage their AUMID and will revert our rewrite
  (known limitation of the non-injection route; see
  `docs/coverage-matrix.md`).

## Evidence & verification

- Implementation plan and task breakdown: archived after full completion
  (2026-10-03) — `docs/plan.md` v2 in git history (commit `bfd7de9`)
- Phase 0b acceptance evidence: [`docs/phase0b-acceptance.md`](docs/phase0b-acceptance.md)
- Coverage matrix and the "B+ stays the main route" ruling:
  [`docs/coverage-matrix.md`](docs/coverage-matrix.md)
- Measured size / memory / stress numbers: [`BENCHMARK.md`](BENCHMARK.md)
- CI (windows-latest, a real interactive Windows session — per the maintainer
  its runs count as real-machine runs): `cargo build --release --locked
  --workspace` + 60+ unit tests + a 104-assertion runtime
  smoke (dual-line AUMID rewrites and restores, startup sweep, interactive
  menu, HKCU-Run autostart, `.lnk` tile pin/unpin with the taskbarpin verb,
  tile↔live-window linkage via UIA, explorer-restart re-sweep, ring log,
  circuit breaker) and a 30+ assertion acceptance suite (50-window
  stress per line, <10 MB memory gate, multi-app coverage, UIA
  taskbar-button dumps). The former injection-edition Phase INJ
  (27 assertions) was removed together with the edition on 2026-10-02
  (plan v2 §0-8).

## Antivirus false positives

tbg-lite ships as a small, stripped, **unsigned** single-file exe, and its
behavior profile is inherently close to what heuristic engines watch for:
rewriting other processes' window properties (documented Shell property-store
API), global `SetWinEventHook` listeners, per-user HKCU-Run autostart, and
taskbar tile pinning. Engines that score behavior and heuristics — Kaspersky
in particular — may therefore raise generic verdicts (e.g.
`UDS:DangerousObject`, `PDM:Trojan.Win32.Generic`, `Heur.…`) on fresh
builds. These are heuristic scores, not identifications of known malware.

Mitigations in place (task 32):

- Every build embeds a version resource (product name, file description,
  version, copyright, project URL) and an application manifest
  (`asInvoker`, Windows 10/11 `supportedOS`) — see `build.rs`.
  Version-less, manifest-less exes score noticeably worse in heuristics.
- Artifacts are built by GitHub Actions from the exact pushed commit and
  carry a build-provenance attestation; `SHA256SUMS.txt` ships with every
  stable release and dev artifact — verify before running.

If Kaspersky flags a copy that matches `SHA256SUMS.txt`:

1. Report it as a false positive via the
   [Kaspersky OpenTip portal](https://opentip.kaspersky.com/) (submit the
   file or its SHA256; confirmed false positives are typically cleared by a
   database update within days), or use the report-a-false-positive action
   in the product's Quarantine/Reports view.
2. Until the verdict is cleared, add an exclusion for the verified file if
   your policy allows it.
3. Prefer the attested release builds over self-built exes — self-built
   unsigned binaries always look more suspicious to heuristics.

A code-signing certificate would remove most of this warning class; it is
not planned for now, so verify-and-report is the supported path.

## Known limitations (route B+)

- Explorer folder windows revert their AUMID (shell-managed). Since task 28
  the watcher **re-applies the marker automatically** (NAMECHANGE check +
  5 s periodic verify); expect a brief button jump each time the shell wins
  a round. Non-injection cannot prevent the shell from rewriting its own
  windows (per Microsoft's AppUserModelIDs docs) — it can only fight back.
- Chromium-family browsers self-manage their AUMID; short-horizon rewrites
  hold, long-horizon behavior is hardware/browser-update dependent (the
  re-assert pass catches most of these too).
- No icon/jumplist "translation layer" (that would require an injection
  route, removed 2026-10-02); suffix-marked windows may show generic jump
  lists.
- New-window race: a button may briefly appear in its native group before the
  rewrite lands (sub-millisecond writes in practice; 0 missed in 50-window
  stress).

## License

MIT — see [LICENSE](LICENSE).
