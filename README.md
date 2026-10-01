# tbg-lite

Taskbar grouping controller for Windows 11 (support scope per the
2026-10-01 maintainer decision — Windows 10 users are served by other
established tools), written in Rust — in **two
editions** (task 33–37, plan v2 §0-6):

- **tbg-lite** (main edition, zero-injection): controls taskbar button
  grouping by rewriting each window's `PKEY_AppUserModel_ID` through the
  documented Shell property-store API (`SHGetPropertyStoreForWindow`) — no
  DLL injection, no shell patching, no admin rights. Single exe, ~1.5 MB
  private working set.
- **tbg-inject** (injection edition, route A): a separate `tbg-inject.exe` +
  `tbg_hook.dll` pair that redirects the same API **inside** explorer via an
  IAT slot patch and a delegating `IPropertyStore`, so the taskbar reads the
  rewritten AUMID in-process while the windows' real properties are never
  touched (no restore table needed — unhook and native grouping returns).
  See "The injection edition" below for usage and its distinct risk profile.

Both editions share the two strategy lines (`watch --strategy` for tbg-lite,
`inject --strategy` for tbg-inject):

- `ungroup` (default) — per-window suffix `~TBG~w<HWND>`; enabling the watch
  ungroups everything on the taskbar, including windows that already existed
  at startup (Windhawk-mod-compatible default).
- `group --group <NAME>` — shared AUMID `TBG.Group.<NAME>` for custom
  grouping; originals are persisted to `%LOCALAPPDATA%\tbg-lite\tbg-restore.tsv`
  (atomic writes, single-instance mutex) for `restore`.

## Installation

- **Stable**: [latest release](https://github.com/xixka/taskbar-grouping/releases/latest)
  (`v*` tags; two zips — `tbg-lite-<tag>-x86_64-windows.zip` single exe,
  `tbg-inject-<tag>-x86_64-windows.zip` exe + `tbg_hook.dll` — each with
  `SHA256SUMS.txt` and its own build-provenance attestation).
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

## The injection edition (tbg-inject)

`tbg-inject.exe` + `tbg_hook.dll` (keep the DLL next to the exe) implement
route A with a clean-room design (plan v2 §5): no private symbols, no
inline hooks, no memory code patches.

```
tbg-inject                              (no arguments: interactive menu)
tbg-inject inject [--strategy ungroup|group] [--group <NAME>]
tbg-inject stop                         (unhook + unload + final counters)
tbg-inject status                       (hook state, patched modules, traffic)
```

How it works — the host locates explorer, loads the DLL into it with
`CreateRemoteThread + LoadLibraryW`, then calls its `tbg_hook_init` export
remotely. The DLL walks explorer's module import tables and redirects every
static import of `shell32!SHGetPropertyStoreForWindow` (plus its
`GetProcAddress`-resolved and delay-load call sites) to a stub; the stub
calls the original and wraps the returned `IPropertyStore` (including the
`IPropertyStoreCache` view) in a delegating object that rewrites only
`PKEY_AppUserModel_ID` reads (line-1 per-window suffix / line-2 shared
AUMID, same markers as tbg-lite). Everything else — `GetCount`, `GetAt`,
`SetValue`, `Commit`, other keys — passes through untouched.

**Windows 11 grouping limitation (known limitation 5, measured on CI
2026-09-30).** The interception above is verified working at the API level
— the taskbar queries the wrapped stores — but on Windows 11 it reads only
`System.Taskbar.TabList` through them and resolves the grouping AUMID via
internal WinRT/CTaskBand paths instead (confirmed by full proxy
instrumentation and by an early-injection experiment on a fresh shell; the
community taskbar-grouping mod achieves the effect only by hooking
Taskbar.dll private symbols, which this project's clean-room rules forbid).
Consequences on Windows 11: the hook installs and reports cleanly, native
grouping is left undisturbed, but window grouping does **not** change while
it is active. The CI asserts this stability property (doubling as a canary:
if a future Windows routes the read through the documented call, the
assertion flips and the limitation is lifted). The classic (Windows 10
style) taskbar was tested once on a Server 2022 runner and showed the same
structure — the taskbar queries the store through the hook (calls/wrapped
>= 1, TabList reads) but takes the grouping AUMID from the window-property
atom fast path — so the limitation is structural, not Windows-11-specific.
Per the 2026-10-01 scope decision the project targets Windows 11 only;
that classic-taskbar CI leg has been retired (the script is kept in
ci/runtime-smoke-inj.ps1 for reproduction). For grouping
changes on Windows 11 today, use the default tbg-lite edition, whose
external AUMID writes are consumed by the same internal pipeline and
are verified end-to-end in CI.

**Upgrading from a dev build earlier than 2026-10-01 (fix round 9).**
Builds before that date issued an extra remote `LoadLibraryW` before the
stop export, so the DLL's reference count never reached zero: `stop`
printed `stop: ok` but `tbg_hook.dll` stayed loaded in explorer (the file
remained locked). The current build makes every `stop` end in a real
unload (asserted against explorer's module list in CI) and adds an
orphan-instance self-heal path. If an old instance is still resident, run
`tbg-inject stop` once with the new build; if it reports the DLL cannot
self-unload, restart explorer (`taskkill /f /im explorer.exe` then
`start explorer`) or reboot once to clear it. Afterwards stop/unload
works normally on every cycle.

Edition boundaries:

- **No autostart, no restore table.** Injection is an explicit, deliberate
  action; because real properties are never written, unhooking is the whole
  "restore".
- **Mutually exclusive with `tbg-lite watch`** — one taskbar, one edition.
  Stop one before starting the other.
- **No icon/jumplist translation** — this edition intercepts AUMID reads,
  not the rest of the property surface.
- If explorer restarts, the hook dies with it: the shared section's
  lifetime is tied to its mappings, so a later `tbg-inject status` sees a
  fresh detached section and a fresh `inject` re-attaches to the new
  explorer.

Risk profile (expect it, plan for it):

- **Antivirus products will likely flag this edition.** Injecting a DLL into
  explorer is a real injection technique, not a heuristic artifact — engines
  are right to score it. The release carries the same
  SHA256SUMS + build-provenance attestation so you can verify the binary
  you run is the one built from this repository; if your engine blocks it,
  that is the product working as designed. Only run it if you accept
  running injection-based tools at all.
- The DLL runs inside explorer: a bug there can take the shell down (CI
  gates the pair end-to-end, but that risk is inherent to the route).
- Unloading has a theoretical in-flight-call window (1.5 s grace period
  before `FreeLibraryAndExitThread`; documented in plan v2 §5).

## Coexistence with Windhawk

tbg-lite never injects into explorer, so it can run alongside Windhawk and its
mods. (The injection edition is the exception: `tbg-inject` occupies the same
in-process territory as Windhawk mods — do not run them against the same
mechanism.) Two things to keep in mind:

- The `taskbar-grouping` Windhawk mod implements the same feature via symbol
  hooks **inside** explorer; running both simultaneously would fight over the
  same windows' AUMIDs. Pick one — if you install the Windhawk mod, stop the
  tbg-lite watch (`menu [3]` or simply don't autostart it) and `restore`.
- Explorer folder windows self-manage their AUMID and will revert our rewrite
  (known limitation of the non-injection route; see
  `docs/coverage-matrix.md`).

## Evidence & verification

- Implementation plan and task breakdown: [`docs/plan.md`](docs/plan.md) (route B+, v2)
- Phase 0b acceptance evidence: [`docs/phase0b-acceptance.md`](docs/phase0b-acceptance.md)
- Coverage matrix and the "B+ stays the main route" ruling:
  [`docs/coverage-matrix.md`](docs/coverage-matrix.md)
- Measured size / memory / stress numbers: [`BENCHMARK.md`](BENCHMARK.md)
- CI (windows-latest, a real interactive Windows session — per the maintainer
  its runs count as real-machine runs): `cargo build --release --locked
  --workspace` (both editions) + 60+ unit tests + a 131-assertion runtime
  smoke (dual-line AUMID rewrites and restores, startup sweep, interactive
  menu, HKCU-Run autostart, `.lnk` tile pin/unpin with the taskbarpin verb,
  tile↔live-window linkage via UIA, explorer-restart re-sweep, ring log,
  circuit breaker, plus the injection-edition Phase INJ end-to-end:
  artifacts, CLI codes, menu, patched-IAT interception with real AUMIDs
  untouched and native grouping undisturbed (known limitation 5, see
  below), interception counters with per-method proxy instrumentation,
  unhook restore, explorer-restart re-inject, and an early-injection
  decisive experiment) and a 30+ assertion acceptance suite (50-window
  stress per line, <10 MB memory gate, multi-app coverage, UIA
  taskbar-button dumps).

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
- No icon/jumplist "translation layer" (that is injection-route capability);
  suffix-marked windows may show generic jump lists.
- New-window race: a button may briefly appear in its native group before the
  rewrite lands (sub-millisecond writes in practice; 0 missed in 50-window
  stress).

## License

MIT — see [LICENSE](LICENSE).
