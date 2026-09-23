# ci/runtime-smoke.ps1 - task 9 + task 13 + task 14 + task 19 (docs/plan.md v2 §3)
#
# Runtime smoke test for both strategy lines, executed on a GitHub Actions
# windows-latest runner, which is a real Windows session. It spawns notepad
# windows, drives `tbg-lite watch/restore`, and asserts the AUMID values
# read back from the live windows:
#   Phase 0 - task 13 startup sweep (line 1, GATING): notepads opened
#             BEFORE the watch starts must get the per-window suffix too
#             (enabling the watch = ungroup everything).
#   Phase A - line 1 (ungroup): every notepad gets a distinct ~TBG~w<HWND>
#             suffix; restore returns every window to its original AUMID.
#   Phase B - line 2 (group): one notepad is opened BEFORE the watch starts
#             (task 13: the startup sweep must pull it into the group);
#             every notepad gets the exact shared AUMID TBG.Group.smoke;
#             restore uses tbg-restore.tsv and the map file is cleaned up
#             afterwards.
#   Phase M - task 14 interactive menu (no arguments): two menu sessions
#             driven entirely by pre-written stdin lines — M1 starts the
#             ungroup watch from the menu, inspects, exits WITHOUT restore
#             (graceful stop: stats printed, rewrites kept); M2 restores
#             from the menu, starts a group watch (name via prompt), stops
#             it, exits. No Ctrl+C involved anywhere.
#   Phase I - task 19 autostart (HKCU Run): install (line 1 default),
#             registry value compared byte-for-byte, status reports all
#             three state blocks, install --strategy group overwrites the
#             value (line 2), uninstall verified idempotent, usage error
#             (group without --group) exits 2.
#   Phase P - task 16 pin (.lnk tile): a shortcut carrying the shared
#             AUMID TBG.Group.<NAME> is generated for a notepad target;
#             the tool self-verifies by reloading the saved file (the
#             'aumid ... verified' output line), the .lnk lands in the
#             default %LOCALAPPDATA%\tbg-lite\pin directory and in a
#             custom --out directory with --icon, re-running replaces the
#             tile, and usage errors (no args / bad group / missing target
#             / nonexistent target / bad icon spec) exit 2.
#   Phase C - explorer/taskbar feasibility probe (best effort, no
#             assertions): screenshots only, to see whether a real taskbar
#             can be hosted in this session.
# Exit code 0 = all assertions passed, 1 = at least one failure.
# All logs and screenshots are written to ci/out/ (uploaded as artifacts).

$ErrorActionPreference = 'Stop'

$root = $PSScriptRoot                      # <repo>\ci
$out  = Join-Path $root 'out'
$exe  = Join-Path $root '..\target\release\tbg-lite.exe'

New-Item -ItemType Directory -Force -Path $out | Out-Null

$failures = New-Object System.Collections.Generic.List[string]
$passes   = New-Object System.Collections.Generic.List[string]

function Log([string]$msg)  { Write-Host "[smoke] $msg" }
function Pass([string]$msg) { $script:passes.Add($msg);   Write-Host "[smoke] PASS: $msg" -ForegroundColor Green }
function Fail([string]$msg) { $script:failures.Add($msg); Write-Host "[smoke] FAIL: $msg" -ForegroundColor Red }
function Assert([bool]$cond, [string]$msg) {
  if ($cond) { Pass $msg } else { Fail $msg }
}

function Shot([string]$name) {
  try {
    Add-Type -AssemblyName System.Windows.Forms
    Add-Type -AssemblyName System.Drawing
    $vs  = [System.Windows.Forms.SystemInformation]::VirtualScreen
    $bmp = New-Object System.Drawing.Bitmap($vs.Width, $vs.Height)
    $g   = [System.Drawing.Graphics]::FromImage($bmp)
    $g.CopyFromScreen($vs.X, $vs.Y, 0, 0, $bmp.Size)
    $bmp.Save((Join-Path $out $name), [System.Drawing.Imaging.ImageFormat]::Png)
    $g.Dispose(); $bmp.Dispose()
    Log "screenshot saved: $name"
  } catch {
    Log "screenshot unavailable ($name): $($_.Exception.Message)"
  }
}

function Get-WindowAumid([UInt64]$hwnd) {
  # Task 25 (audit BUG-14): machine-readable JSON instead of regexing
  # the human table (title lines could steal the AUMID match).
  $hex = '0x{0:X}' -f $hwnd
  $j = & $exe inspect --hwnd $hex --json | ConvertFrom-Json
  if ($LASTEXITCODE -ne 0) { throw "inspect --hwnd $hex --json exited with code $LASTEXITCODE" }
  if ($null -eq $j -or $null -eq $j.aumid) { throw "no aumid in JSON output for $hex" }
  return [string]$j.aumid
}

function Spawn-Notepads([int]$n) {
  $list = @()
  for ($i = 0; $i -lt $n; $i++) {
    $p = Start-Process -FilePath 'notepad.exe' -PassThru
    $deadline = (Get-Date).AddSeconds(20)
    while (-not $p.HasExited -and $p.MainWindowHandle -eq 0 -and (Get-Date) -lt $deadline) {
      Start-Sleep -Milliseconds 250
      $p.Refresh()
    }
    if ($p.HasExited -or $p.MainWindowHandle -eq 0) {
      throw "notepad #$i did not create a main window (is this an interactive session?)"
    }
    # IntPtr has ToInt64() (not ToUInt64); the numeric Int64->UInt64 cast
    # works on both Windows PowerShell 5.1 and pwsh 7 (handles are positive).
    $list += [pscustomobject]@{ Proc = $p; Hwnd = [UInt64]$p.MainWindowHandle.ToInt64() }
    Start-Sleep -Milliseconds 400
  }
  return $list
}

function Clear-TestWindows {
  Get-Process -Name 'notepad'  -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
  Get-Process -Name 'tbg-lite' -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
}

function Start-Watch([string[]]$watchArgs, [string]$logName, [string[]]$StdinLines) {
  $logPath = Join-Path $out $logName
  $errPath = Join-Path $out ($logName -replace '\.log$', '.err.log')
  # Round 4: Start-Process -PassThru proved unreliable on the runner - the
  # returned Process object's ExitCode stayed empty even after a successful
  # timed WaitForExit (rounds 2-3). Start the process through
  # System.Diagnostics.Process directly instead: we own the handle from the
  # start (cached below while the process is alive), the pipes are drained by
  # async readers (no pipe-full deadlock), and the parameterless
  # WaitForExit() in Wait-Watch is documented to guarantee a readable
  # ExitCode. The exit-code assertion itself is unchanged (still fails on
  # null / non-zero) - no verification was weakened.
  # Task 14: $StdinLines (optional) pre-writes scripted input for the
  # interactive-menu sessions (no-args launch) and then closes the pipe;
  # EOF makes the menu exit gracefully should it ever read past the script.
  $quoted = $watchArgs | ForEach-Object {
    if ($_ -match '[\s"]') { '"' + ($_ -replace '"', '\"') + '"' } else { $_ }
  }
  $psi = New-Object System.Diagnostics.ProcessStartInfo
  $psi.FileName               = $exe
  $psi.Arguments              = ($quoted -join ' ')
  $psi.UseShellExecute        = $false
  $psi.RedirectStandardOutput = $true
  $psi.RedirectStandardError  = $true
  $psi.RedirectStandardInput  = ($null -ne $StdinLines)
  $psi.CreateNoWindow         = $true
  $p = New-Object System.Diagnostics.Process
  $p.StartInfo = $psi
  $null = $p.Start()
  $null = $p.Handle  # cache a full-access handle while the process is alive
  $outTask = $p.StandardOutput.ReadToEndAsync()
  $errTask = $p.StandardError.ReadToEndAsync()
  if ($null -ne $StdinLines) {
    foreach ($l in $StdinLines) { $p.StandardInput.WriteLine($l) }
    $p.StandardInput.Flush()
    $p.StandardInput.Close()
  }
  $p | Add-Member -NotePropertyName OutTask -NotePropertyValue $outTask
  $p | Add-Member -NotePropertyName ErrTask -NotePropertyValue $errTask
  $p | Add-Member -NotePropertyName OutPath -NotePropertyValue $logPath
  $p | Add-Member -NotePropertyName ErrPath -NotePropertyValue $errPath
  return $p
}

function Flush-WatchLogs([System.Diagnostics.Process]$proc) {
  # Copy the asynchronously captured stdout/stderr into the artifact log
  # files (replaces the Start-Process file redirection). Called on both the
  # normal-exit and the kill/timeout paths so logs survive either way.
  try {
    $proc.OutTask.Result | Set-Content $proc.OutPath -Encoding UTF8
    $proc.ErrTask.Result | Set-Content $proc.ErrPath -Encoding UTF8
  } catch {
    Log "watch log flush warning: $($_.Exception.Message)"
  }
}

function Wait-Watch([System.Diagnostics.Process]$proc, [int]$timeoutSec) {
  if (-not $proc.WaitForExit($timeoutSec * 1000)) {
    $proc.Kill()
    $proc.WaitForExit()
    Flush-WatchLogs $proc
    throw "watch process did not exit within $timeoutSec s"
  }
  # The parameterless WaitForExit() additionally waits for the redirected
  # pipes to drain and completes exit-code retrieval; per the .NET docs this
  # is the only form that guarantees ExitCode is populated afterwards.
  $proc.WaitForExit()
  Flush-WatchLogs $proc
  $code = $proc.ExitCode
  if ($null -eq $code -or $code -ne 0) {
    throw "watch exited with code $code"
  }
}

# ---------------------------------------------------------------- env report
$envLines = @(
  "time     : $(Get-Date -Format s)",
  "computer : $env:COMPUTERNAME",
  "user     : $([Environment]::UserName)",
  "session  : $env:SESSIONNAME",
  "os       : $([Environment]::OSVersion.VersionString)",
  "explorer : $(if (Get-Process -Name explorer -ErrorAction SilentlyContinue) { 'running' } else { 'not running' })"
)
$envLines | ForEach-Object { Log $_ }
$envLines | Set-Content (Join-Path $out 'env.txt') -Encoding UTF8
& $exe --version | Set-Content (Join-Path $out 'version.txt') -Encoding UTF8

# --------------- phase 0: task 13 startup sweep (line 1, pre-existing)
try {
  Log '=== Phase 0: startup sweep of pre-existing windows (line 1, task 13) ==='
  # Two notepads opened BEFORE the watch starts: the startup sweep must
  # rewrite them too (default behavior = ungroup everything on enable).
  $preNotepads = Spawn-Notepads 2
  Start-Sleep -Seconds 1
  $watch = Start-Watch @('watch','--duration','10','--strategy','ungroup','--verbose') 'watch-line1-sweep.log'
  Wait-Watch $watch 60
  $watchLog = Get-Content (Join-Path $out 'watch-line1-sweep.log') -Raw
  & $exe inspect --all | Set-Content (Join-Path $out 'inspect-line1-sweep.log') -Encoding UTF8

  $aumids = @()
  foreach ($w in $preNotepads) { $aumids += (Get-WindowAumid $w.Hwnd) }
  for ($i = 0; $i -lt $preNotepads.Count; $i++) {
    Log ("pre-existing notepad #{0} hwnd=0x{1:X} aumid='{2}'" -f ($i+1), $preNotepads[$i].Hwnd, $aumids[$i])
  }
  Assert (@($aumids | Where-Object { $_ -cmatch '~TBG~w[0-9A-F]{1,16}$' }).Count -eq 2) 'line1-sweep: both PRE-EXISTING notepads got the per-window suffix'
  Assert (@($aumids | Select-Object -Unique).Count -eq 2) 'line1-sweep: suffixes are pairwise distinct'
  if ($watchLog -match 'startup sweep \(task 13\): pre-existing windows rewritten=(\d+)') {
    Assert ([int]$Matches[1] -ge 2) "line1-sweep: startup sweep rewrote >= 2 windows (stats: $($Matches[1]))"
  } else {
    Fail 'line1-sweep: watch log missing the startup-sweep stats line'
  }
  Assert ($watchLog -cmatch 'SWEEP 0x[0-9A-F]+') 'line1-sweep: sweep activity visible in the watch log (SWEEP events)'
  Shot 'desktop-line1-sweep.png'

  # restore and verify originals (original = current value minus the suffix)
  $expected = @()
  for ($i = 0; $i -lt $preNotepads.Count; $i++) {
    $expected += ($aumids[$i] -creplace '~TBG~w[0-9A-F]{1,16}$', '')
  }
  $restoreOut = & $exe restore | Out-String
  $restoreOut | Set-Content (Join-Path $out 'restore-line1-sweep.log') -Encoding UTF8
  Assert ($LASTEXITCODE -eq 0) 'line1-sweep: restore command exited 0'
  $after = @()
  foreach ($w in $preNotepads) { $after += (Get-WindowAumid $w.Hwnd) }
  $okCount = 0
  for ($i = 0; $i -lt $preNotepads.Count; $i++) {
    if ($after[$i] -eq $expected[$i]) { $okCount++ }
  }
  Assert ($okCount -eq 2) "line1-sweep: restore returned both pre-existing notepads to their original AUMIDs ($okCount/2)"
} catch {
  Fail "phase 0 crashed: $($_.Exception.Message)"
  Log $_.ScriptStackTrace
} finally {
  Clear-TestWindows
}

# ------------------------------------------------- phase A: line 1 (ungroup)
try {
  Log '=== Phase A: watch --strategy ungroup (line 1) ==='
  $watch = Start-Watch @('watch','--duration','25','--strategy','ungroup','--verbose') 'watch-line1-ungroup.log'
  Start-Sleep -Seconds 2
  $notepads = Spawn-Notepads 3
  Wait-Watch $watch 90
  $watchLog = Get-Content (Join-Path $out 'watch-line1-ungroup.log') -Raw
  & $exe inspect --all | Set-Content (Join-Path $out 'inspect-line1.log') -Encoding UTF8

  $aumids = @()
  foreach ($w in $notepads) { $aumids += (Get-WindowAumid $w.Hwnd) }
  for ($i = 0; $i -lt $notepads.Count; $i++) {
    Log ("notepad #{0} hwnd=0x{1:X} aumid='{2}'" -f ($i+1), $notepads[$i].Hwnd, $aumids[$i])
  }
  Assert (@($aumids | Where-Object { $_ -cmatch '~TBG~w[0-9A-F]{1,16}$' }).Count -eq 3) 'line1: all 3 notepad windows carry the per-window suffix'
  Assert (@($aumids | Select-Object -Unique).Count -eq 3) 'line1: suffixes are pairwise distinct (one taskbar group per window)'
  if ($watchLog -match 'rewritten\s+:\s+(\d+)') {
    Assert ([int]$Matches[1] -ge 3) "line1: watch rewrote at least 3 windows (stats say $($Matches[1]))"
  } else {
    Fail 'line1: watch stats did not report a rewritten count'
  }
  $missedNotepad = @(($watchLog -split "`n") | Where-Object { $_ -match 'missed: ' -and $_ -match 'class=Notepad' })
  Assert ($missedNotepad.Count -eq 0) 'line1: no notepad window was missed'

  Shot 'desktop-line1-ungroup.png'

  # restore and verify originals (original = current value minus the suffix)
  $expected = @()
  for ($i = 0; $i -lt $notepads.Count; $i++) {
    $expected += ($aumids[$i] -creplace '~TBG~w[0-9A-F]{1,16}$', '')
  }
  $restoreOut = & $exe restore | Out-String
  $restoreOut | Set-Content (Join-Path $out 'restore-line1.log') -Encoding UTF8
  Assert ($LASTEXITCODE -eq 0) 'line1: restore command exited 0'
  $after = @()
  foreach ($w in $notepads) { $after += (Get-WindowAumid $w.Hwnd) }
  $okCount = 0
  for ($i = 0; $i -lt $notepads.Count; $i++) {
    if ($after[$i] -eq $expected[$i]) { $okCount++ }
  }
  Assert ($okCount -eq 3) "line1: restore returned all 3 notepads to their original AUMIDs ($okCount/3)"
} catch {
  Fail "phase A crashed: $($_.Exception.Message)"
  Log $_.ScriptStackTrace
} finally {
  Clear-TestWindows
}

# -------------------------------------------------- phase B: line 2 (group)
try {
  Log '=== Phase B: watch --strategy group --group smoke (line 2) ==='
  # Task 22 (audit SEC-01): the restore map lives in %LOCALAPPDATA%\tbg-lite
  $mapPath = Join-Path $env:LOCALAPPDATA 'tbg-lite\tbg-restore.tsv'
  if (Test-Path $mapPath) { Remove-Item $mapPath -Force }  # start clean
  # Task 13: one notepad opened BEFORE the watch starts - the startup sweep
  # must pull it into the shared group and persist its original to the map.
  $preNotepads = Spawn-Notepads 1
  $origPre = Get-WindowAumid $preNotepads[0].Hwnd
  Log ("pre-existing notepad hwnd=0x{0:X} original aumid='{1}'" -f $preNotepads[0].Hwnd, $origPre)
  $watch = Start-Watch @('watch','--duration','25','--strategy','group','--group','smoke','--verbose') 'watch-line2-group.log'
  Start-Sleep -Seconds 2
  $newNotepads = Spawn-Notepads 2
  $notepads = @($preNotepads) + @($newNotepads)
  Wait-Watch $watch 90
  $watchLog = Get-Content (Join-Path $out 'watch-line2-group.log') -Raw
  & $exe inspect --all | Set-Content (Join-Path $out 'inspect-line2.log') -Encoding UTF8

  $aumids = @()
  foreach ($w in $notepads) { $aumids += (Get-WindowAumid $w.Hwnd) }
  for ($i = 0; $i -lt $notepads.Count; $i++) {
    Log ("notepad #{0} hwnd=0x{1:X} aumid='{2}'" -f ($i+1), $notepads[$i].Hwnd, $aumids[$i])
  }
  Assert ((Get-WindowAumid $preNotepads[0].Hwnd) -eq 'TBG.Group.smoke') 'line2-sweep: the PRE-EXISTING notepad was pulled into the shared group at startup'
  Assert (@($aumids | Where-Object { $_ -eq 'TBG.Group.smoke' }).Count -eq 3) 'line2: all 3 notepad windows (incl. pre-existing) carry the exact shared AUMID'
  Assert (@($aumids | Select-Object -Unique).Count -eq 1) 'line2: one identical AUMID across all windows (single taskbar group)'

  # expected originals come from the restore map written next to the exe
  Assert (Test-Path $mapPath) 'line2: restore map file was created next to the exe'
  $preKey = '{0:X}' -f $preNotepads[0].Hwnd
  $preMapped = $false
  $origByHwnd = @{}
  if (Test-Path $mapPath) {
    Get-Content $mapPath | ForEach-Object {
      $f = $_ -split "`t"
      if ($f.Count -ge 3) {
        $origByHwnd[$f[0]] = $f[2]
        if ($f[0] -eq $preKey) { $preMapped = $true }
      }
    }
  }
  Assert $preMapped 'line2-sweep: restore map holds the pre-existing notepad original'
  Shot 'desktop-line2-group.png'

  $restoreOut = & $exe restore | Out-String
  $restoreOut | Set-Content (Join-Path $out 'restore-line2.log') -Encoding UTF8
  Assert ($LASTEXITCODE -eq 0) 'line2: restore command exited 0'
  $after = @(); $expected = @()
  for ($i = 0; $i -lt $notepads.Count; $i++) {
    $after += (Get-WindowAumid $notepads[$i].Hwnd)
    $key = '{0:X}' -f $notepads[$i].Hwnd
    $expected += [string]$origByHwnd[$key]
  }
  $okCount = 0
  for ($i = 0; $i -lt $notepads.Count; $i++) {
    if ($after[$i] -eq $expected[$i]) { $okCount++ }
  }
  Assert ($okCount -eq 3) "line2: restore returned all 3 notepads to their mapped originals ($okCount/3)"
  Assert (-not (Test-Path $mapPath)) 'line2: restore map file removed after full restore'
} catch {
  Fail "phase B crashed: $($_.Exception.Message)"
  Log $_.ScriptStackTrace
} finally {
  Clear-TestWindows
}

# --------------------------- phase M: interactive menu (task 14, no args)
# Maintainer rework (2026-09-22): no-arguments launch opens the interactive
# menu; exit goes through menu option [0] - no Ctrl+C anywhere. Both
# sessions are driven entirely by pre-written stdin lines (Start-Watch
# closes the pipe afterwards; menu EOF fallback exits gracefully anyway).
# NOTE (run 35698563610): the .NET StandardInput StreamWriter emits a UTF-8
# BOM on its FIRST write, so the first scripted line arrives as "<BOM>4".
# The menu strips a leading U+FEFF (it is not trim()-whitespace); the
# first-action assertions below (menu1 '1', menu2 '4') regression-guard
# that BOM tolerance.
try {
  Log '=== Phase M: interactive menu, no arguments (task 14) ==='

  # --- session M1: menu-driven ungroup watch; exit WITHOUT restore ---
  $mNotepads = Spawn-Notepads 2
  Start-Sleep -Seconds 1
  $mOrig = @()
  foreach ($w in $mNotepads) { $mOrig += (Get-WindowAumid $w.Hwnd) }
  # '1' start ungroup watch / '5' inspect / '0' exit / 'n' keep rewrites
  $menu1 = Start-Watch @() 'menu-line1-ungroup.log' @('1','5','0','n')
  Wait-Watch $menu1 60
  $menu1Log = Get-Content (Join-Path $out 'menu-line1-ungroup.log') -Raw

  Assert ($menu1Log -cmatch 'interactive menu') 'menu1: no-args launch shows the interactive menu'
  if ($menu1Log -match 'startup sweep \(task 13\): pre-existing windows rewritten=(\d+)') {
    Assert ([int]$Matches[1] -ge 2) "menu1: menu-started watch swept >= 2 pre-existing windows (stats: $($Matches[1]))"
  } else {
    Fail 'menu1: watch log missing the startup-sweep stats line'
  }
  Assert ($menu1Log -cmatch 'watch: stop requested') 'menu1: graceful stop requested via the menu was logged'
  Assert ($menu1Log -cmatch '==== watch stats') 'menu1: watch stats printed on graceful stop'
  Assert ($menu1Log -cmatch 'HWND\s+PID\s+CLASS') 'menu1: menu [5] printed the inspect table'
  $m1Aumids = @()
  foreach ($w in $mNotepads) { $m1Aumids += (Get-WindowAumid $w.Hwnd) }
  Assert (@($m1Aumids | Where-Object { $_ -cmatch '~TBG~w[0-9A-F]{1,16}$' }).Count -eq 2) 'menu1: exit without restore keeps the line-1 suffixes (stop keeps rewrites)'

  # --- session M2: menu restore, menu-driven group watch, stop, exit ---
  $mapPath = Join-Path $env:LOCALAPPDATA 'tbg-lite\tbg-restore.tsv'
  if (Test-Path $mapPath) { Remove-Item $mapPath -Force }  # start clean
  # '4' restore / 'y' confirm / '2' group watch / 'smoke' name / '3' stop / '0' exit
  $menu2 = Start-Watch @() 'menu-line2-group.log' @('4','y','2','smoke','3','0')
  Wait-Watch $menu2 60
  $menu2Log = Get-Content (Join-Path $out 'menu-line2-group.log') -Raw

  # Both counters are successful line-1 restores: restored = original AUMID
  # written back; cleared = the window had NO original AUMID (empty), so the
  # property is cleared (VT_EMPTY). On this runner the notepads have an empty
  # native AUMID, so they count as cleared (run 35699360388: restored=0
  # cleared=3). The per-window end-state assertions below are unaffected.
  if ($menu2Log -match 'restore summary: restored=(\d+) cleared=(\d+)') {
    Assert (([int]$Matches[1] + [int]$Matches[2]) -ge 2) "menu2: menu [4] restored >= 2 windows (summary: restored=$($Matches[1]) cleared=$($Matches[2]))"
  } else {
    Fail 'menu2: menu restore summary line missing'
  }
  if ($menu2Log -match 'startup sweep \(task 13\): pre-existing windows rewritten=(\d+)') {
    Assert ([int]$Matches[1] -ge 2) "menu2: menu group watch swept >= 2 pre-existing windows (stats: $($Matches[1]))"
  } else {
    Fail 'menu2: group watch log missing the startup-sweep stats line'
  }
  $m2Aumids = @()
  foreach ($w in $mNotepads) { $m2Aumids += (Get-WindowAumid $w.Hwnd) }
  Assert (@($m2Aumids | Where-Object { $_ -eq 'TBG.Group.smoke' }).Count -eq 2) 'menu2: both notepads carry the shared group AUMID after the menu session'
  $mappedCount = 0
  if (Test-Path $mapPath) {
    Get-Content $mapPath | ForEach-Object {
      $f = $_ -split "`t"
      if ($f.Count -ge 3) {
        foreach ($w in $mNotepads) { if ($f[0] -eq ('{0:X}' -f $w.Hwnd)) { $mappedCount++ } }
      }
    }
  }
  Assert ($mappedCount -eq 2) 'menu2: restore map holds both notepad originals'

  # --- cleanup via the CLI (args mode unchanged): restore + map removal ---
  $restoreOut = & $exe restore | Out-String
  $restoreOut | Set-Content (Join-Path $out 'restore-menu.log') -Encoding UTF8
  Assert ($LASTEXITCODE -eq 0) 'menu: CLI restore cleanup exited 0'
  $mFinal = @()
  foreach ($w in $mNotepads) { $mFinal += (Get-WindowAumid $w.Hwnd) }
  $okCount = 0
  for ($i = 0; $i -lt $mNotepads.Count; $i++) {
    if ($mFinal[$i] -eq $mOrig[$i]) { $okCount++ }
  }
  Assert ($okCount -eq 2) "menu: final CLI restore returned both notepads to their original AUMIDs ($okCount/2)"
  Assert (-not (Test-Path $mapPath)) 'menu: restore map removed after full restore'
  Shot 'desktop-menu-after.png'
} catch {
  Fail "phase M crashed: $($_.Exception.Message)"
  Log $_.ScriptStackTrace
} finally {
  Clear-TestWindows
}

# --------------------------- phase I: install / uninstall / status (task 19)
# HKCU Run autostart, both strategy lines in turn: registry value data is
# compared byte-for-byte against the expected command (quoted exe + watch
# tail), `status` must report all three state blocks, uninstall is verified
# idempotent, and usage errors (group without --group) must exit 2.
try {
  Log '=== Phase I: install / uninstall / status (task 19, HKCU Run) ==='
  $runKeyPath = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run'
  # defensive pre-clean (not an assertion): never inherit a stale value
  if (Get-ItemProperty -Path $runKeyPath -Name 'tbg-lite' -ErrorAction SilentlyContinue) {
    Remove-ItemProperty -Path $runKeyPath -Name 'tbg-lite' -ErrorAction SilentlyContinue
  }

  # --- baseline: nothing installed ---
  $st0 = & $exe status | Out-String
  Assert (($LASTEXITCODE -eq 0) -and ($st0 -cmatch 'autostart\s*:\s*not installed')) 'status: baseline reports autostart not installed (exit 0)'
  Assert ($st0 -cmatch 'marked\s*:\s*line1\(ungroup\)=\d+\s+line2\(group\)=\d+') 'status: marked-window counters present for both strategy lines'
  Assert ($st0 -cmatch 'restore map\s*:') 'status: restore map state line present'

  # --- line 1 (default ungroup) ---
  $exeReal = (Resolve-Path $exe).Path
  $cmd1 = '"' + $exeReal + '" watch --strategy ungroup --duration 0'
  $install1 = & $exe install | Out-String
  Assert (($LASTEXITCODE -eq 0) -and ($install1 -cmatch 'autostart registered')) 'install: line-1 install exits 0 and reports registration'
  $reg1 = (Get-ItemProperty -Path $runKeyPath -Name 'tbg-lite' -ErrorAction SilentlyContinue).'tbg-lite'
  Assert ($reg1 -eq $cmd1) "install: HKCU Run value data matches the line-1 command exactly (got: $reg1)"
  $st1 = & $exe status | Out-String
  Assert (($LASTEXITCODE -eq 0) -and ($st1 -cmatch 'autostart\s*:\s*installed') -and ($st1 -cmatch [regex]::Escape('--strategy ungroup --duration 0'))) 'status: reports installed with the line-1 ungroup command'

  # --- line 2 (group) overwrites the value ---
  $cmd2 = '"' + $exeReal + '" watch --strategy group --group smoke --duration 0'
  $install2 = & $exe install --strategy group --group smoke | Out-String
  Assert (($LASTEXITCODE -eq 0) -and ($install2 -cmatch 'autostart updated')) 'install: line-2 re-install reports overwrite (updated)'
  $reg2 = (Get-ItemProperty -Path $runKeyPath -Name 'tbg-lite' -ErrorAction SilentlyContinue).'tbg-lite'
  Assert ($reg2 -eq $cmd2) "install: HKCU Run value switched to the line-2 group command (got: $reg2)"
  $st2 = & $exe status | Out-String
  Assert (($LASTEXITCODE -eq 0) -and ($st2 -cmatch [regex]::Escape('--strategy group --group smoke --duration 0'))) 'status: reports the line-2 group command'

  # --- uninstall (verified idempotent) ---
  $un = & $exe uninstall | Out-String
  Assert (($LASTEXITCODE -eq 0) -and ($un -cmatch 'autostart removed')) 'uninstall: exits 0 and reports removal'
  Assert ($null -eq (Get-ItemProperty -Path $runKeyPath -Name 'tbg-lite' -ErrorAction SilentlyContinue)) 'uninstall: HKCU Run value is gone'
  $un2 = & $exe uninstall | Out-String
  Assert (($LASTEXITCODE -eq 0) -and ($un2 -cmatch 'not installed')) 'uninstall: second run exits 0 with nothing to remove (idempotent)'

  # --- usage error: --strategy group without --group must exit 2 ---
  # (Start-Watch + manual wait: Wait-Watch throws on non-zero exit, which is
  # exactly what we WANT to observe here; stderr lands in the .err.log file)
  $usage = Start-Watch @('install','--strategy','group') 'install-usage.log'
  $null = $usage.WaitForExit(30000)
  $usage.WaitForExit()
  Flush-WatchLogs $usage
  Assert ($usage.ExitCode -eq 2) "install usage: --strategy group without --group exits 2 (got: $($usage.ExitCode))"
  $usageErr = Get-Content (Join-Path $out 'install-usage.err.log') -Raw
  Assert ($usageErr -cmatch 'usage:') 'install usage: stderr carries the usage message'
} catch {
  Fail "phase I crashed: $($_.Exception.Message)"
  Log $_.ScriptStackTrace
} finally {
  # never leave the autostart value behind on the runner
  Remove-ItemProperty -Path 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run' -Name 'tbg-lite' -ErrorAction SilentlyContinue
}

# --------------------------- phase P: pin .lnk tile generation (task 16)
# The tile shortcut carries the line-2 shared AUMID so that pinned tiles
# and live windows merge (task 18 asserts the live-window side). Here:
# happy paths (default dir, --icon + --out custom dir, overwrite) all
# gated on the read-back verification line; usage errors exit 2.
try {
  Log '=== Phase P: pin - .lnk tile with the group AUMID (task 16) ==='
  $notepadExe = Join-Path $env:WINDIR 'System32\notepad.exe'
  $shell32    = Join-Path $env:WINDIR 'System32\shell32.dll'
  $pinDir     = Join-Path $env:LOCALAPPDATA 'tbg-lite\pin'
  $pinPath    = Join-Path $pinDir 'pinsmoke.lnk'
  # defensive pre-clean (not an assertion): never inherit a stale tile
  Remove-Item $pinPath -ErrorAction SilentlyContinue

  # --- usage errors: exit code 2 (stderr captured via Start-Watch) ---
  $u1 = Start-Watch @('pin') 'pin-usage-noargs.log'
  $null = $u1.WaitForExit(30000); $u1.WaitForExit(); Flush-WatchLogs $u1
  Assert ($u1.ExitCode -eq 2) "pin usage: no arguments exits 2 (got $($u1.ExitCode))"
  $u1err = Get-Content (Join-Path $out 'pin-usage-noargs.err.log') -Raw
  Assert ($u1err -cmatch 'usage:') 'pin usage: no arguments, stderr carries the usage message'

  $u2 = Start-Watch @('pin','--group','pinsmoke') 'pin-usage-notarget.log'
  $null = $u2.WaitForExit(30000); $u2.WaitForExit(); Flush-WatchLogs $u2
  Assert ($u2.ExitCode -eq 2) "pin usage: missing --target exits 2 (got $($u2.ExitCode))"

  $u3 = Start-Watch @('pin','--group','bad name','--target',$notepadExe) 'pin-usage-badgroup.log'
  $null = $u3.WaitForExit(30000); $u3.WaitForExit(); Flush-WatchLogs $u3
  Assert ($u3.ExitCode -eq 2) "pin usage: invalid group name exits 2 (got $($u3.ExitCode))"

  $u4 = Start-Watch @('pin','--group','pinsmoke','--target','C:\definitely\missing.exe') 'pin-usage-badtarget.log'
  $null = $u4.WaitForExit(30000); $u4.WaitForExit(); Flush-WatchLogs $u4
  Assert ($u4.ExitCode -eq 2) "pin usage: nonexistent --target exits 2 (got $($u4.ExitCode))"

  # --- happy path 1: default output directory, no --icon ---
  $pin1 = & $exe pin --group pinsmoke --target "$notepadExe" | Out-String
  Assert (($LASTEXITCODE -eq 0) -and ($pin1 -cmatch 'aumid\s*:\s*TBG\.Group\.pinsmoke') -and ($pin1 -cmatch 'verified')) 'pin: default output exits 0, AUMID read back from the .lnk and verified'
  Assert (Test-Path $pinPath) "pin: tile created at the default location ($pinPath)"
  Assert ((Test-Path $pinPath) -and ((Get-Item $pinPath).Length -gt 0)) 'pin: tile .lnk file is non-empty'

  # --- happy path 2: --icon with index + custom --out directory ---
  $customDir = Join-Path $out 'pin-custom'
  $pin2 = & $exe pin --group pinsmoke --target "$notepadExe" --icon "$shell32,2" --out "$customDir" | Out-String
  Assert (($LASTEXITCODE -eq 0) -and ($pin2 -cmatch 'verified')) 'pin: --icon + --out exits 0 and verifies'
  Assert (Test-Path (Join-Path $customDir 'pinsmoke.lnk')) 'pin: tile written to the custom --out directory'

  # --- overwrite: re-run replaces the existing tile (install semantics) ---
  $pin3 = & $exe pin --group pinsmoke --target "$notepadExe" --icon "$shell32" | Out-String
  Assert (($LASTEXITCODE -eq 0) -and ($pin3 -cmatch 'replaced existing file') -and ($pin3 -cmatch 'verified')) 'pin: re-run overwrites the existing tile and still verifies'

  # --- bad icon spec: file-not-found surfaces as a usage error ---
  $u5 = Start-Watch @('pin','--group','pinsmoke','--target',$notepadExe,'--icon','C:\no\such.ico,zz') 'pin-usage-badicon.log'
  $null = $u5.WaitForExit(30000); $u5.WaitForExit(); Flush-WatchLogs $u5
  Assert ($u5.ExitCode -eq 2) "pin usage: nonexistent --icon file exits 2 (got $($u5.ExitCode))"
} catch {
  Fail "phase P crashed: $($_.Exception.Message)"
  Log $_.ScriptStackTrace
} finally {
  # hygiene: never leave test tiles behind on the runner (not an assertion)
  Remove-Item (Join-Path $env:LOCALAPPDATA 'tbg-lite\pin\pinsmoke.lnk') -ErrorAction SilentlyContinue
}

# --------------------------- phase C: explorer/taskbar feasibility probe
# Best effort only (no assertions): can this session host a real taskbar?
try {
  Log '=== Phase C: explorer/taskbar feasibility probe (best effort) ==='
  $hadExplorer = [bool](Get-Process -Name explorer -ErrorAction SilentlyContinue)
  Log "explorer running at probe start: $hadExplorer"
  if (-not $hadExplorer) {
    try {
      Start-Process -FilePath 'explorer.exe' -ErrorAction Stop
      Start-Sleep -Seconds 5
    } catch {
      Log "explorer launch failed: $($_.Exception.Message)"
    }
  }
  $inspectAll = & $exe inspect --all | Out-String
  $inspectAll | Set-Content (Join-Path $out 'inspect-phaseC.log') -Encoding UTF8
  $tray = @(($inspectAll -split "`n") | Where-Object { $_ -match 'Shell_TrayWnd' })
  Log ("taskbar window (Shell_TrayWnd) present: {0}" -f ($tray.Count -gt 0))
  if ($tray.Count -gt 0) {
    Shot 'desktop-phaseC-taskbar-baseline.png'
    $watch = Start-Watch @('watch','--duration','15','--strategy','group','--group','visual') 'watch-phaseC-visual.log'
    Start-Sleep -Seconds 2
    $notepads = Spawn-Notepads 3
    Wait-Watch $watch 60
    Shot 'desktop-phaseC-taskbar-grouped.png'
    & $exe restore | Set-Content (Join-Path $out 'restore-phaseC.log') -Encoding UTF8
    Shot 'desktop-phaseC-taskbar-restored.png'
    Log 'visual evidence captured (grouped vs restored taskbar screenshots)'
  } else {
    Log 'no taskbar in this session; visual taskbar verification not possible here (AUMID behavior is already asserted in phases A/B)'
  }
} catch {
  Log "phase C probe issue (non-fatal): $($_.Exception.Message)"
} finally {
  Clear-TestWindows
}

# ------------------------------------------------------------------- summary
$result = @()
$result += "tbg-lite runtime smoke - $(Get-Date -Format s)"
$result += "session  : $env:SESSIONNAME"
$result += "passed   : $($passes.Count)"
$result += "failed   : $($failures.Count)"
$result += ''
$result += $passes
$result += ''
if ($failures.Count -gt 0) { $result += $failures }
$result | Set-Content (Join-Path $out 'RESULT.txt') -Encoding UTF8

Log "RESULT: $($passes.Count) passed, $($failures.Count) failed (details in ci/out/RESULT.txt)"
if ($failures.Count -gt 0) { exit 1 }
exit 0
