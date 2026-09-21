# ci/runtime-smoke.ps1 - task 9 (docs/plan.md Phase 0b-(9))
#
# Runtime smoke test for both strategy lines, executed on a GitHub Actions
# windows-latest runner, which is a real Windows session. It spawns notepad
# windows, drives `tbg-lite watch/restore`, and asserts the AUMID values
# read back from the live windows:
#   Phase A - line 1 (ungroup): every notepad gets a distinct ~TBG~w<HWND>
#             suffix; restore returns every window to its original AUMID.
#   Phase B - line 2 (group):  every notepad gets the exact shared AUMID
#             TBG.Group.smoke; restore uses tbg-restore.tsv and the map
#             file is cleaned up afterwards.
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
  $hex  = '0x{0:X}' -f $hwnd
  $text = & $exe inspect --hwnd $hex | Out-String
  if ($LASTEXITCODE -ne 0) { throw "inspect --hwnd $hex exited with code $LASTEXITCODE" }
  if ($text -match 'AUMID\s+:\s*(.*)') {
    $v = $Matches[1].Trim()
    if ($v -eq '<empty>') { return '' }
    return $v
  }
  throw "could not parse AUMID from inspect output for $hex"
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

function Start-Watch([string[]]$watchArgs, [string]$logName) {
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
  $mapPath = Join-Path (Split-Path $exe -Parent) 'tbg-restore.tsv'
  if (Test-Path $mapPath) { Remove-Item $mapPath -Force }  # start clean
  $watch = Start-Watch @('watch','--duration','25','--strategy','group','--group','smoke','--verbose') 'watch-line2-group.log'
  Start-Sleep -Seconds 2
  $notepads = Spawn-Notepads 3
  Wait-Watch $watch 90
  $watchLog = Get-Content (Join-Path $out 'watch-line2-group.log') -Raw
  & $exe inspect --all | Set-Content (Join-Path $out 'inspect-line2.log') -Encoding UTF8

  $aumids = @()
  foreach ($w in $notepads) { $aumids += (Get-WindowAumid $w.Hwnd) }
  for ($i = 0; $i -lt $notepads.Count; $i++) {
    Log ("notepad #{0} hwnd=0x{1:X} aumid='{2}'" -f ($i+1), $notepads[$i].Hwnd, $aumids[$i])
  }
  Assert (@($aumids | Where-Object { $_ -eq 'TBG.Group.smoke' }).Count -eq 3) 'line2: all 3 notepad windows carry the exact shared AUMID'
  Assert (@($aumids | Select-Object -Unique).Count -eq 1) 'line2: one identical AUMID across all windows (single taskbar group)'

  # expected originals come from the restore map written next to the exe
  Assert (Test-Path $mapPath) 'line2: restore map file was created next to the exe'
  $origByHwnd = @{}
  if (Test-Path $mapPath) {
    Get-Content $mapPath | ForEach-Object {
      $f = $_ -split "`t"
      if ($f.Count -ge 3) { $origByHwnd[$f[0]] = $f[2] }
    }
  }
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
