# ci/phase0b-accept.ps1 - task 10 (docs/plan.md v1 Phase 0b acceptance)
#
# Phase 0b acceptance suite for both strategy lines (route B+, zero
# injection), run on a GitHub Actions windows-latest runner (a real
# interactive Windows session). Produces the quantitative acceptance
# evidence required by the old plan's Phase 0b gate:
#
#   Phase A - line 1 (ungroup), 50-window stress (GATING):
#             every notepad window carries a pairwise-distinct per-window
#             suffix; watch stats report rewritten>=50, alive=50,
#             missed=0, reverted=0; restore returns every window.
#   Phase B - line 2 (group), 50-window stress (GATING):
#             every notepad window carries the exact shared AUMID
#             TBG.Group.smoke; the restore map covers every discovered
#             window one-to-one (task 13 startup sweep may add extra
#             entries for windows that pre-date the watch, e.g. the
#             runner's own console) and is removed after a full restore.
#   Phase C - resident memory (GATING): tbg-lite working set stays under
#             10 MB while watching (acceptance line: <10 MB).
#   Phase D - multi-app coverage subset, line 1: notepad/mspaint/cmd
#             assert-if-spawned; explorer folder windows log-only (shell
#             windows may self-manage AUMID by design - evidence only).
#             Task 15 (2026-09-23, maintainer: CI runs count as real-machine
#             runs): + Windows PowerShell console / regedit (assert-if-
#             spawned) to broaden the coverage matrix executed on the
#             runner; powershell is cleaned up by PID (killing it by name
#             would kill the CI step itself). NOTE: spawning wt.exe (Windows
#             Terminal) here was tried and DETERMINISTICALLY killed the
#             runner 3x (runs 35810343047 / 35811737548 / 35812271701: WT is
#             single-instance; force-killing the hosting WindowsTerminal.exe
#               takes the session console with it) - WT coverage stays a
#               maintainer real-machine item instead.
#   Phase E - Edge/Chromium revert probe (LOG-ONLY, OPT-IN via
#             TBG_EDGE_PROBE=1): Chromium self-manages its AUMID; probe
#             whether our rewrite survives 6 s after the watch exits.
#             Skipped in CI by default: the runner died with a shutdown
#             signal at this point 4x (see the phase header). Historical
#             evidence: run 35679966357, docs/phase0b-acceptance.md.
#   UIA     - taskbar button dumps via Windows PowerShell 5.1 UIAutomation
#             during line-1 stress / line-2 group / after restore
#             (LOG-ONLY): taskbar-level evidence beyond AUMID values.
#
# Exit code 0 = all gated assertions passed; 1 = at least one failure.
# All logs/screenshots go to ci/out/ (uploaded as artifacts).

$ErrorActionPreference = 'Stop'

$root = $PSScriptRoot                      # <repo>\ci
$out  = Join-Path $root 'out'
$exe  = Join-Path $root '..\target\release\tbg-lite.exe'

New-Item -ItemType Directory -Force -Path $out | Out-Null

$failures = New-Object System.Collections.Generic.List[string]
$passes   = New-Object System.Collections.Generic.List[string]

function Log([string]$msg)  { Write-Host "[acc] $msg" }
function Pass([string]$msg) { $script:passes.Add($msg);   Write-Host "[acc] PASS: $msg" -ForegroundColor Green }
function Fail([string]$msg) { $script:failures.Add($msg); Write-Host "[acc] FAIL: $msg" -ForegroundColor Red }
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

# --- process control (same approach as the fixed runtime-smoke.ps1) --------
# Start-Process -PassThru proved unreliable for ExitCode on the runner, so
# watch is started through System.Diagnostics.Process directly: we own the
# handle from the start, pipes are drained by async readers, and the
# parameterless WaitForExit() guarantees a readable ExitCode.

function Start-Watch([string[]]$watchArgs, [string]$logName) {
  $logPath = Join-Path $out $logName
  $errPath = Join-Path $out ($logName -replace '\.log$', '.err.log')
  $quoted = $watchArgs | ForEach-Object {
    if ($_ -match '[\s"]') { '"' + ($_ -replace '"', '\"') + '"' } else { $_ }
  }
  $psi = New-Object System.Diagnostics.ProcessStartInfo
  $psi.FileName               = $exe
  $psi.Arguments              = ($quoted -join ' ')
  $psi.UseShellExecute        = $false
  $psi.RedirectStandardOutput = $true
  $psi.RedirectStandardError  = $true
  $psi.CreateNoWindow         = $true
  $p = New-Object System.Diagnostics.Process
  $p.StartInfo = $psi
  $null = $p.Start()
  $null = $p.Handle  # cache a full-access handle while the process is alive
  $outTask = $p.StandardOutput.ReadToEndAsync()
  $errTask = $p.StandardError.ReadToEndAsync()
  $p | Add-Member -NotePropertyName OutTask -NotePropertyValue $outTask
  $p | Add-Member -NotePropertyName ErrTask -NotePropertyValue $errTask
  $p | Add-Member -NotePropertyName OutPath -NotePropertyValue $logPath
  $p | Add-Member -NotePropertyName ErrPath -NotePropertyValue $errPath
  return $p
}

function Flush-WatchLogs([System.Diagnostics.Process]$proc) {
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
  $proc.WaitForExit()
  Flush-WatchLogs $proc
  $code = $proc.ExitCode
  if ($null -eq $code -or $code -ne 0) {
    throw "watch exited with code $code"
  }
}

function Clear-TestWindows {
  foreach ($n in @('notepad', 'mspaint', 'cmd', 'tbg-lite', 'msedge', 'regedit')) {
    Get-Process -Name $n -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
  }
}

# Task 15: processes that must NEVER be killed by name (powershell.exe /
# WindowsTerminal.exe host the CI step itself) - collected during Phase D
# discovery and terminated by PID in the phase's finally block.
$specialPids = @()

# --- app-window discovery via our own `inspect --json` (task 25, audit
# BUG-14: fixed-width column slicing broke on non-ASCII titles; JSON is
# immune and also survives localized content) ------------------------------

function Get-AppWindowList {
  $list = @()
  # Task 25 fix round 1: capture the raw output first and parse the whole
  # JSON document with -InputObject (avoids PS 5.1 pipeline per-item
  # binding surprises on native exe output). The raw text is also archived
  # for diagnosis.
  $rawLines = & $exe inspect --json
  if ($LASTEXITCODE -ne 0) { throw "inspect --json exited with code $LASTEXITCODE" }
  $raw = ($rawLines -join "`n").Trim()
  if ($raw.Length -eq 0) { return $list }
  try { $raw | Set-Content (Join-Path $out 'inspect-json-raw.txt') -Encoding UTF8 } catch {}
  $rows = @(ConvertFrom-Json -InputObject $raw)
  foreach ($item in $rows) {
    # Fix round 2: on this runner's Windows PowerShell 5.1, ConvertFrom-Json
    # returns the parsed JSON array NESTED inside a single-element wrapper
    # (observed in runs 35689677042 / 35690197028: every $w.hwnd was the
    # space-joined enumeration of ALL windows). Flatten one level; both
    # shapes (plain array / nested wrapper) are handled identically.
    $candidates = if ($item -is [System.Array]) { $item } else { @($item) }
    foreach ($w in $candidates) {
      if ($null -eq $w -or $null -eq $w.hwnd) { continue }
      $t = [string]$w.hwnd
      if (-not $t.StartsWith('0x')) { continue }
      $hex = $t.Substring(2)
      if ($hex -notmatch '^[0-9A-F]+$') {
        Log "Get-AppWindowList: skipping row with malformed hwnd: '$t'"
        continue
      }
      $list += [pscustomobject]@{
        Hex   = $hex
        Hwnd  = [Convert]::ToUInt64($hex, 16)
        Pid   = $w.pid
        Class = [string]$w.class
        Aumid = [string]$w.aumid
        Title = [string]$w.title
      }
    }
  }
  return $list
}

function Test-WinMatch($w, [string[]]$classPats, [string[]]$titlePats) {
  foreach ($p in $classPats) { if ($p -and ($w.Class -like $p)) { return $true } }
  foreach ($p in $titlePats) { if ($p -and ($w.Title -like $p)) { return $true } }
  return $false
}

function Wait-NewWindows($beforeHex, [int]$count, [string[]]$classPats, [string[]]$titlePats, [int]$timeoutSec) {
  $found = @()
  $deadline = (Get-Date).AddSeconds($timeoutSec)
  while ((Get-Date) -lt $deadline) {
    $cur = @(Get-AppWindowList)
    $found = @($cur | Where-Object {
      ($beforeHex -notcontains $_.Hex) -and (Test-WinMatch $_ $classPats $titlePats)
    })
    if ($found.Count -ge $count) { return $found }
    Start-Sleep -Milliseconds 500
  }
  return $found
}

function Get-WatchStat([string]$log, [string]$pattern) {
  $m = [regex]::Match($log, $pattern)
  if ($m.Success) { return [int]$m.Groups[1].Value }
  return -1
}

function Get-WatchLog([string]$logName) {
  return (Get-Content (Join-Path $out $logName) -Raw)
}

# --- UIA taskbar button dump (log-only, via Windows PowerShell 5.1) --------

function Dump-TaskbarButtons([string]$name) {
  try {
    $uia = @'
Add-Type -AssemblyName UIAutomationClient
Add-Type -AssemblyName UIAutomationTypes
$root = [System.Windows.Automation.AutomationElement]::RootElement
$cond = New-Object System.Windows.Automation.PropertyCondition([System.Windows.Automation.AutomationElement]::ControlTypeProperty, [System.Windows.Automation.ControlType]::Button)
$btns = $root.FindAll([System.Windows.Automation.TreeScope]::Descendants, $cond)
foreach ($b in $btns) { $n = $b.Current.Name; if ($n) { Write-Output $n } }
'@
    $names = @(& powershell.exe -NoProfile -Command $uia)
    ($names -join "`r`n") | Set-Content (Join-Path $out "taskbar-buttons-$name.txt") -Encoding UTF8
    $notepadBtns = @($names | Where-Object { $_ -match 'Notepad' })
    Log ("UIA taskbar buttons '{0}': {1} names total, {2} mentioning Notepad" -f $name, $names.Count, $notepadBtns.Count)
  } catch {
    Log "UIA taskbar buttons '$name' unavailable: $($_.Exception.Message) (non-fatal)"
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

# Task 22 (audit SEC-01): the restore map lives in %LOCALAPPDATA%\tbg-lite
$mapPath = Join-Path $env:LOCALAPPDATA 'tbg-lite\tbg-restore.tsv'
if (Test-Path $mapPath) { Remove-Item $mapPath -Force }   # start clean

# --------------------------- Phase A: line 1 (ungroup) 50-window stress ----
try {
  Log '=== Phase A: line 1 (ungroup) - 50-window stress ==='
  $watch = Start-Watch @('watch', '--duration', '75', '--strategy', 'ungroup') 'acc-watch-line1-stress.log'
  Start-Sleep -Seconds 2
  $before = @(Get-AppWindowList | ForEach-Object { $_.Hex })
  for ($i = 0; $i -lt 50; $i++) {
    Start-Process -FilePath 'notepad.exe'
    Start-Sleep -Milliseconds 250
  }
  $wins = @(Wait-NewWindows $before 50 @('Notepad') @() 45)
  Log ("line1 stress: {0}/50 notepad windows discovered" -f $wins.Count)
  Wait-Watch $watch 150
  $log = Get-WatchLog 'acc-watch-line1-stress.log'
  $n = $wins.Count
  Assert ($n -eq 50) "line1 stress: 50/50 notepad windows discovered (got $n)"
  $rew    = Get-WatchStat $log 'rewritten\s+:\s+(\d+)'
  $alive  = Get-WatchStat $log 'alive with marker\s+:\s+(\d+)'
  $revert = Get-WatchStat $log 'reverted \(app rewrote its AUMID\)\s+:\s+(\d+)'
  $missed = Get-WatchStat $log 'missed \(new app w/o marker\)\s+:\s+(\d+)'
  Assert ($rew -ge $n)    "line1 stress: watch rewrote >= $n windows (stats: $rew)"
  Assert ($alive -ge $n)  "line1 stress: >= $n windows alive with marker at watch end (stats: $alive)"
  Assert ($revert -eq 0) "line1 stress: no window reverted its AUMID (stats: $revert)"
  Assert ($missed -eq 0) "line1 stress: no window missed (stats: $missed)"
  $aumidByHex = @{}
  foreach ($w in $wins) { $aumidByHex[$w.Hex] = (Get-WindowAumid $w.Hwnd) }
  $marked = @($aumidByHex.Keys | ForEach-Object { $aumidByHex[$_] } | Where-Object { $_ -cmatch '~TBG~w[0-9A-F]{1,16}$' })
  Assert ($marked.Count -eq $n) "line1 stress: all $n windows carry the per-window suffix"
  $distinct = @($aumidByHex.Keys | ForEach-Object { $aumidByHex[$_] } | Select-Object -Unique)
  Assert ($distinct.Count -eq $n) "line1 stress: suffixes pairwise distinct ($($distinct.Count) unique AUMIDs)"
  Dump-TaskbarButtons 'line1-stress'
  Shot 'desktop-acc-line1-stress.png'
  $restoreOut = & $exe restore | Out-String
  $restoreOut | Set-Content (Join-Path $out 'acc-restore-line1.log') -Encoding UTF8
  Assert ($LASTEXITCODE -eq 0) 'line1 stress: restore exited 0'
  $ok = 0
  foreach ($w in $wins) {
    $after = Get-WindowAumid $w.Hwnd
    $expected = $aumidByHex[$w.Hex] -creplace '~TBG~w[0-9A-F]{1,16}$', ''
    if ($after -eq $expected) { $ok++ }
  }
  Assert ($ok -eq $n) "line1 stress: restore returned all windows to their original AUMIDs ($ok/$n)"
} catch {
  Fail "phase A crashed: $($_.Exception.Message)"
  Log $_.ScriptStackTrace
} finally {
  Clear-TestWindows
}

# --------------------------- Phase B: line 2 (group) 50-window stress ------
try {
  Log '=== Phase B: line 2 (group) - 50-window stress ==='
  if (Test-Path $mapPath) { Remove-Item $mapPath -Force }
  $watch = Start-Watch @('watch', '--duration', '75', '--strategy', 'group', '--group', 'smoke') 'acc-watch-line2-stress.log'
  Start-Sleep -Seconds 2
  $before = @(Get-AppWindowList | ForEach-Object { $_.Hex })
  for ($i = 0; $i -lt 50; $i++) {
    Start-Process -FilePath 'notepad.exe'
    Start-Sleep -Milliseconds 250
  }
  $wins = @(Wait-NewWindows $before 50 @('Notepad') @() 45)
  Log ("line2 stress: {0}/50 notepad windows discovered" -f $wins.Count)
  Wait-Watch $watch 150
  $log = Get-WatchLog 'acc-watch-line2-stress.log'
  $n = $wins.Count
  Assert ($n -eq 50) "line2 stress: 50/50 notepad windows discovered (got $n)"
  $rew    = Get-WatchStat $log 'rewritten\s+:\s+(\d+)'
  $alive  = Get-WatchStat $log 'alive with marker\s+:\s+(\d+)'
  $revert = Get-WatchStat $log 'reverted \(app rewrote its AUMID\)\s+:\s+(\d+)'
  $missed = Get-WatchStat $log 'missed \(new app w/o marker\)\s+:\s+(\d+)'
  Assert ($rew -ge $n)    "line2 stress: watch rewrote >= $n windows (stats: $rew)"
  Assert ($alive -ge $n)  "line2 stress: >= $n windows alive with marker at watch end (stats: $alive)"
  Assert ($revert -eq 0) "line2 stress: no window reverted its AUMID (stats: $revert)"
  Assert ($missed -eq 0) "line2 stress: no window missed (stats: $missed)"
  $aumidByHex = @{}
  foreach ($w in $wins) { $aumidByHex[$w.Hex] = (Get-WindowAumid $w.Hwnd) }
  $shared = @($aumidByHex.Keys | ForEach-Object { $aumidByHex[$_] } | Where-Object { $_ -eq 'TBG.Group.smoke' })
  Assert ($shared.Count -eq $n) "line2 stress: all $n windows carry the exact shared AUMID TBG.Group.smoke"
  Assert (Test-Path $mapPath) 'line2 stress: restore map file created next to the exe'
  $entries = @()
  if (Test-Path $mapPath) { $entries = @(Get-Content $mapPath | Where-Object { $_.Trim().Length -gt 0 }) }
  # Task 13: the startup sweep also groups windows that existed before the
  # watch started (e.g. the runner's console), so the map legitimately holds
  # MORE than the 50 discovered windows. The precise invariant is one entry
  # per discovered window; the total is asserted to be at least that.
  $entryHex = @($entries | ForEach-Object { ($_ -split "`t")[0] })
  $mapped = @($wins | Where-Object { $entryHex -contains $_.Hex })
  Assert ($mapped.Count -eq $n) "line2 stress: restore map covers every discovered window ($($mapped.Count)/$n)"
  Assert ($entries.Count -ge $n) "line2 stress: restore map has >= $n entries (got $($entries.Count); extras are startup-sweep windows)"
  $origByHex = @{}
  foreach ($e in $entries) {
    $f = $e -split "`t"
    if ($f.Count -ge 3) { $origByHex[$f[0]] = $f[2] }
  }
  Dump-TaskbarButtons 'line2-group'
  Shot 'desktop-acc-line2-group.png'
  $restoreOut = & $exe restore | Out-String
  $restoreOut | Set-Content (Join-Path $out 'acc-restore-line2.log') -Encoding UTF8
  Assert ($LASTEXITCODE -eq 0) 'line2 stress: restore exited 0'
  $ok = 0
  foreach ($w in $wins) {
    $after = Get-WindowAumid $w.Hwnd
    $expected = [string]$origByHex[$w.Hex]
    if ($after -eq $expected) { $ok++ }
  }
  Assert ($ok -eq $n) "line2 stress: restore returned all windows to their mapped originals ($ok/$n)"
  Assert (-not (Test-Path $mapPath)) 'line2 stress: restore map removed after full restore'
  Dump-TaskbarButtons 'post-restore'
  Shot 'desktop-acc-post-restore.png'
} catch {
  Fail "phase B crashed: $($_.Exception.Message)"
  Log $_.ScriptStackTrace
} finally {
  Clear-TestWindows
}

# --------------------------- Phase C: resident memory ----------------------
try {
  Log '=== Phase C: resident memory under load ==='
  $watch = Start-Watch @('watch', '--duration', '25', '--strategy', 'ungroup') 'acc-watch-memory.log'
  Start-Sleep -Seconds 2
  for ($i = 0; $i -lt 3; $i++) {
    Start-Process -FilePath 'notepad.exe'
    Start-Sleep -Milliseconds 400
  }
  Start-Sleep -Seconds 10   # let events process, then settle
  $watch.Refresh()
  $ws   = $watch.WorkingSet64
  $priv = $watch.PrivateMemorySize64
  Log ("memory: tbg-lite working set = {0:N0} bytes ({1:N2} MB); private = {2:N0} bytes ({3:N2} MB)" -f $ws, ($ws / 1MB), $priv, ($priv / 1MB))
  ("working_set_bytes={0}`r`nprivate_bytes={1}" -f $ws, $priv) | Set-Content (Join-Path $out 'memory.txt') -Encoding UTF8
  Assert ($ws -lt 10MB) 'memory: working set below 10 MB (Phase 0b acceptance line)'
  Wait-Watch $watch 60
  $log = Get-WatchLog 'acc-watch-memory.log'
  $rew = Get-WatchStat $log 'rewritten\s+:\s+(\d+)'
  Log ("memory: watch rewrote {0} window(s) during the sample" -f $rew)
} catch {
  Fail "phase C crashed: $($_.Exception.Message)"
  Log $_.ScriptStackTrace
} finally {
  Clear-TestWindows
}

# --------------------------- Phase D: multi-app coverage subset ------------
try {
  Log '=== Phase D: multi-app coverage subset (line 1) ==='
  $watch = Start-Watch @('watch', '--duration', '150', '--strategy', 'ungroup', '--verbose') 'acc-watch-multiapp.log'
  Start-Sleep -Seconds 2
  $specs = @(
    @{ Name = 'notepad';  Count = 2; LogOnly = $false
       Classes = @('Notepad'); Titles = @()
       Launch = { Start-Process -FilePath 'notepad.exe' } },
    @{ Name = 'mspaint';  Count = 2; LogOnly = $false
       Classes = @('MSPaintView', '*Paint*'); Titles = @('*Paint*')
       Launch = { Start-Process -FilePath 'mspaint.exe' } },
    @{ Name = 'cmd';      Count = 2; LogOnly = $false
       Classes = @('*ConsoleWindowClass*', '*CASCADIA*'); Titles = @()
       Launch = { Start-Process -FilePath 'cmd.exe' } },
    @{ Name = 'powershell-console'; Count = 2; LogOnly = $false
       Classes = @('*ConsoleWindowClass*', '*CASCADIA*'); Titles = @()
       Launch = { Start-Process -FilePath 'powershell.exe' -ArgumentList '-NoExit' } },
    @{ Name = 'regedit';  Count = 1; LogOnly = $false
       Classes = @('RegEdit_RegEdit'); Titles = @()
       Launch = { Start-Process -FilePath 'regedit.exe' } },
    @{ Name = 'explorer'; Count = 2; LogOnly = $true
       Classes = @('*CabinetWClass*'); Titles = @()
       Launch = { Start-Process -FilePath 'explorer.exe' -ArgumentList "`"$env:TEMP`"" } }
  )
  $results = @()
  foreach ($s in $specs) {
    $before = @(Get-AppWindowList | ForEach-Object { $_.Hex })
    $spawned = $true
    try {
      for ($i = 0; $i -lt $s.Count; $i++) { & $s.Launch; Start-Sleep -Milliseconds 800 }
    } catch {
      Log ("multi-app '{0}': launch failed: {1} - skipped" -f $s.Name, $_.Exception.Message)
      $spawned = $false
    }
    $found = @()
    if ($spawned) { $found = @(Wait-NewWindows $before $s.Count $s.Classes $s.Titles 15) }
    if ($found.Count -eq 0) {
      Log ("multi-app '{0}': no window appeared within 15 s (unavailable on this image) - skipped" -f $s.Name)
    } else {
      Log ("multi-app '{0}': {1}/{2} window(s) discovered" -f $s.Name, $found.Count, $s.Count)
      $results += [pscustomobject]@{ Name = $s.Name; LogOnly = $s.LogOnly; Wins = $found }
      # 任务 15：不能按名杀的进程（powershell 宿主着 CI 步骤本身）收集
      # PID，阶段末按 PID 定点清理
      if ($s.Name -in @('powershell-console')) {
        foreach ($w in $found) { $specialPids += $w.Pid }
      }
    }
  }
  Wait-Watch $watch 220
  $log = Get-WatchLog 'acc-watch-multiapp.log'
  $missed = Get-WatchStat $log 'missed \(new app w/o marker\)\s+:\s+(\d+)'
  Assert ($missed -eq 0) "multi-app: no discovered window missed by watch (stats: $missed)"
  foreach ($r in $results) {
    $aumByHex = @{}
    foreach ($w in $r.Wins) { $aumByHex[$w.Hex] = (Get-WindowAumid $w.Hwnd) }
    $r | Add-Member -NotePropertyName Aumids -NotePropertyValue $aumByHex -Force
    foreach ($hex in $aumByHex.Keys) {
      Log ("multi-app '{0}': 0x{1} aumid='{2}'" -f $r.Name, $hex, $aumByHex[$hex])
    }
    if (-not $r.LogOnly) {
      $marked = @($aumByHex.Keys | ForEach-Object { $aumByHex[$_] } | Where-Object { $_ -cmatch '~TBG~w[0-9A-F]{1,16}$' })
      Assert ($marked.Count -eq $r.Wins.Count) ("multi-app '{0}': all {1} window(s) carry the ungroup suffix" -f $r.Name, $r.Wins.Count)
    }
  }
  $restoreOut = & $exe restore | Out-String
  $restoreOut | Set-Content (Join-Path $out 'acc-restore-multiapp.log') -Encoding UTF8
  Assert ($LASTEXITCODE -eq 0) 'multi-app: restore exited 0'
  foreach ($r in $results) {
    if ($r.LogOnly) { continue }
    $ok = 0
    foreach ($w in $r.Wins) {
      $after = Get-WindowAumid $w.Hwnd
      $expected = [string]$r.Aumids[$w.Hex] -creplace '~TBG~w[0-9A-F]{1,16}$', ''
      if ($after -eq $expected) { $ok++ }
    }
    Assert ($ok -eq $r.Wins.Count) ("multi-app '{0}': restore returned windows to original AUMIDs ({1}/{2})" -f $r.Name, $ok, $r.Wins.Count)
  }
} catch {
  Fail "phase D crashed: $($_.Exception.Message)"
  Log $_.ScriptStackTrace
} finally {
  # task 15: kill the by-name-unsafe processes by PID first (powershell /
  # WindowsTerminal host this very CI step), then the usual by-name sweep
  foreach ($p in $specialPids) {
    Stop-Process -Id $p -Force -ErrorAction SilentlyContinue
  }
  Clear-TestWindows
}

# --------------------------- Phase E: Edge/Chromium revert probe ----------
# Log-only by design: Chromium self-manages its AUMID and may revert our
# rewrite - this is exactly the risk the acceptance report needs evidence
# for, not a gate. NO gated assertions live here.
#
# FIX ROUND 2 (2026-09-23): the runner died with a shutdown signal at this
# exact point 4x in a row (runs 35810343047 / 35811737548 / 35812271701 /
# 35813057776 - always ~25 ms after the phase banner, BEFORE Edge even
# launches, with zero FAIL assertions and all gated phases A-D green;
# removing wt.exe did not help). The historical Edge evidence from
# run 35679966357 is preserved in docs/phase0b-acceptance.md. The phase is
# now OPT-IN via TBG_EDGE_PROBE=1 (workflow_dispatch can set it); CI runs
# skip it so the gated suite stays usable. Not a weakening: this phase
# never carried a gate.
if ($env:TBG_EDGE_PROBE -eq '1') {
try {
  Log '=== Phase E: Edge/Chromium revert probe (log-only, opt-in) ==='
  $watch = Start-Watch @('watch', '--duration', '60', '--strategy', 'ungroup', '--verbose') 'acc-watch-edge.log'
  Start-Sleep -Seconds 2
  $before = @(Get-AppWindowList | ForEach-Object { $_.Hex })
  $launched = $true
  try {
    for ($i = 0; $i -lt 2; $i++) {
      Start-Process -FilePath 'msedge.exe' -ArgumentList @('--new-window', 'about:blank', '--no-first-run', '--no-default-browser-check', ('--user-data-dir=' + $env:TEMP + '\edge-acc-profile'))
      Start-Sleep -Seconds 2
    }
  } catch {
    Log "edge: launch failed: $($_.Exception.Message) - probe skipped (non-fatal)"
    $launched = $false
  }
  $wins = @()
  if ($launched) { $wins = @(Wait-NewWindows $before 2 @('*Chrome_WidgetWin_1*') @() 30) }
  Wait-Watch $watch 90
  if ($wins.Count -eq 0) {
    Log 'edge: no Chrome_WidgetWin_1 window discovered - probe inconclusive on this image (non-fatal)'
  } else {
    Log ("edge: {0} window(s) discovered" -f $wins.Count)
    $t0 = @{}
    foreach ($w in $wins) { $t0[$w.Hex] = (Get-WindowAumid $w.Hwnd) }
    Start-Sleep -Seconds 6
    $t1 = @{}
    foreach ($w in $wins) { $t1[$w.Hex] = (Get-WindowAumid $w.Hwnd) }
    $probeLines = @()
    foreach ($w in $wins) {
      $a0 = [string]$t0[$w.Hex]; $a1 = [string]$t1[$w.Hex]
      $m0 = $a0 -cmatch '~TBG~w[0-9A-F]{1,16}$'; $m1 = $a1 -cmatch '~TBG~w[0-9A-F]{1,16}$'
      $line = ("edge 0x{0}: at-watch-end aumid='{1}' marked={2}; +6s aumid='{3}' marked={4}" -f $w.Hex, $a0, $m0, $a1, $m1)
      $probeLines += $line
      Log $line
    }
    $probeLines | Set-Content (Join-Path $out 'edge-revert-probe.txt') -Encoding UTF8
    $restoreOut = & $exe restore | Out-String
    $restoreOut | Set-Content (Join-Path $out 'acc-restore-edge.log') -Encoding UTF8
    Log "edge: restore exit code $LASTEXITCODE (log-only probe)"
  }
} catch {
  Log "phase E probe issue (non-fatal): $($_.Exception.Message)"
} finally {
  Clear-TestWindows
}
} else {
  Log 'Phase E: Edge/Chromium revert probe SKIPPED (opt-in only, TBG_EDGE_PROBE=1; runner shutdown signal killed the job here 4x - see header note; historical evidence: run 35679966357 in docs/phase0b-acceptance.md)'
}

# ------------------------------------------------------------------- summary
$result = @()
$result += "tbg-lite Phase 0b acceptance - $(Get-Date -Format s)"
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
