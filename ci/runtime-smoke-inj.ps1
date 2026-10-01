# ci/runtime-smoke-inj.ps1 - task 36 follow-up (docs/plan.md v2 SS5, known
# limitation 5 follow-through)
#
# Injection-edition smoke suite for the CLASSIC (Windows 10-style) taskbar,
# executed on a GitHub Actions windows-2022 runner (Windows Server 2022
# ships the classic taskbar; windows-latest = Server 2025 uses the Win11
# taskbar, where limitation 5 applies: the grouping AUMID read bypasses
# SHGetPropertyStoreForWindow).
#
# This is the one environment where route A can plausibly work end to end:
# the classic taskbar groups windows per app user model ID and is expected
# to query the window property store through the documented call. Phases:
#   CI0 - environment probe: OS caption, notepad/mspaint availability,
#         explorer pid (log-only evidence).
#   CI1 - baseline: two notepads, count taskbar buttons (UIA). Expected 1
#         on the classic default (always combine). The count is LOGGED and
#         becomes the reference for the "native returns" assertions.
#   CI2 - line-1 inject (ungroup): inject, open two fresh notepads, count
#         buttons -- expect TWO (route A rewrites AUMID reads in-process
#         and the classic taskbar consumes them). Status counters assert
#         calls/wrapped/aumid-served >= 1 (the API-level proof that the
#         classic taskbar reads PKEY_AppUserModel_ID through the hook).
#         The real window AUMID must stay untouched (route A never writes).
#         Stop -> native grouping returns.
#   CI3 - line-2 inject (group, cross-app): baseline notepad + mspaint =
#         two buttons; inject --strategy group -> both apps served the
#         shared AUMID -> ONE button. Stop -> back to two.
#   CI4 - explorer restart: hook dies with the shell, status reports a
#         fresh detached section, re-inject works, stop is clean.
#
# Result accounting matches runtime-smoke.ps1: RESULT.txt + exit code 1 on
# any failure. The hosting job is marked continue-on-error for its first
# runs (probe status): if the classic taskbar also bypasses the documented
# call, the failure output is the evidence that finalizes the limitation
# wording; if it passes, the job is promoted to a gating leg.

$ErrorActionPreference = 'Stop'
Set-Location (Split-Path $PSScriptRoot -Parent)
$exe     = Join-Path (Get-Location) 'target\release\tbg-lite.exe'
$exeInj  = Join-Path (Get-Location) 'target\release\tbg-inject.exe'
$dllHook = Join-Path (Get-Location) 'target\release\tbg_hook.dll'
$out     = Join-Path $PSScriptRoot 'out'
New-Item -ItemType Directory -Force -Path $out | Out-Null

$script:passes   = New-Object System.Collections.ArrayList
$script:failures = New-Object System.Collections.ArrayList

function Log([string]$msg)  { Write-Host "[smoke-inj] $msg" }
function Pass([string]$msg) { $script:passes.Add($msg);   Write-Host "[smoke-inj] PASS: $msg" -ForegroundColor Green }
function Fail([string]$msg) { $script:failures.Add($msg); Write-Host "[smoke-inj] FAIL: $msg" -ForegroundColor Red }
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

function Get-TaskbarButtonNames {
  $uia = @'
Add-Type -AssemblyName UIAutomationClient
Add-Type -AssemblyName UIAutomationTypes
$root = [System.Windows.Automation.AutomationElement]::RootElement
$cond = New-Object System.Windows.Automation.PropertyCondition([System.Windows.Automation.AutomationElement]::ControlTypeProperty, [System.Windows.Automation.ControlType]::Button)
$btns = $root.FindAll([System.Windows.Automation.TreeScope]::Descendants, $cond)
foreach ($b in $btns) { $n = $b.Current.Name; if ($n) { Write-Output $n } }
'@
  try {
    return @(& powershell.exe -NoProfile -Command $uia)
  } catch {
    Log "UIA button enumeration failed: $($_.Exception.Message)"
    return @()
  }
}

# Poll the taskbar buttons until the count for a name pattern is stable for
# two consecutive reads (classic taskbar updates asynchronously) or the
# deadline passes; returns the last count seen.
function Wait-ButtonCount([string]$pattern, [int]$expect, [int]$seconds) {
  $deadline = (Get-Date).AddSeconds($seconds)
  $last = -1
  $stable = 0
  while ((Get-Date) -lt $deadline) {
    $btns = Get-TaskbarButtonNames
    $c = @($btns | Where-Object { $_ -like $pattern }).Count
    if ($c -eq $last) { $stable++ } else { $stable = 0 }
    $last = $c
    if ($c -eq $expect -and $stable -ge 1) { return $c }
    Start-Sleep -Milliseconds 700
  }
  return $last
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
    $list += [pscustomobject]@{ Proc = $p; Hwnd = [UInt64]$p.MainWindowHandle.ToInt64() }
  }
  return $list
}

function Spawn-Mspaint {
  $p = Start-Process -FilePath 'mspaint.exe' -PassThru
  $deadline = (Get-Date).AddSeconds(25)
  while (-not $p.HasExited -and $p.MainWindowHandle -eq 0 -and (Get-Date) -lt $deadline) {
    Start-Sleep -Milliseconds 250
    $p.Refresh()
  }
  if ($p.HasExited -or $p.MainWindowHandle -eq 0) {
    throw "mspaint did not create a main window (is this an interactive session?)"
  }
  return $p
}

function Clear-TestWindows {
  Get-Process -Name 'notepad'  -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
  Get-Process -Name 'mspaint'  -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
}

function Restart-ExplorerShell {
  $before = (Get-Process -Name explorer -ErrorAction SilentlyContinue | Select-Object -First 1).Id
  Stop-Process -Name explorer -Force -ErrorAction SilentlyContinue
  $deadline = (Get-Date).AddSeconds(30)
  while ((Get-Date) -lt $deadline) {
    Start-Sleep -Seconds 2
    $procs = @(Get-Process -Name explorer -ErrorAction SilentlyContinue)
    if ($procs.Count -gt 0) {
      $newId = ($procs | Select-Object -First 1).Id
      if ($newId -ne $before) { return $newId }
    }
  }
  Log 'Restart-ExplorerShell: auto-restart did not happen; launching explorer.exe manually'
  Start-Process -FilePath 'explorer.exe'
  Start-Sleep -Seconds 5
  $p = Get-Process -Name explorer -ErrorAction SilentlyContinue | Select-Object -First 1
  if ($p) { return $p.Id }
  return 0
}

function Get-WindowAumid([UInt64]$hwnd) {
  $hex = '0x{0:X}' -f $hwnd
  $j = & $exe inspect --hwnd $hex --json | ConvertFrom-Json
  if ($LASTEXITCODE -ne 0) { throw "inspect --hwnd $hex --json exited with code $LASTEXITCODE" }
  if ($null -eq $j -or $null -eq $j.aumid) { throw "no aumid in JSON output for $hex" }
  return [string]$j.aumid
}

function Test-HookDllLoaded {
  # Fix round 9 regression guard: assert the dll's real module-list presence
  # so a leaked LoadLibraryW reference can never hide behind "stop: ok".
  try {
    $procs = @(Get-Process -Name explorer -ErrorAction SilentlyContinue)
    foreach ($p in $procs) {
      try {
        foreach ($m in $p.Modules) {
          if ($m.ModuleName -ieq 'tbg_hook.dll') { return $true }
        }
      } catch { }
    }
  } catch { }
  return $false
}

function Wait-HookDllGone([int]$seconds) {
  $deadline = (Get-Date).AddSeconds($seconds)
  while ((Get-Date) -lt $deadline) {
    if (-not (Test-HookDllLoaded)) { return $true }
    Start-Sleep -Milliseconds 500
  }
  return (-not (Test-HookDllLoaded))
}

try {
  # ---------------- preflight ----------------
  Assert (Test-Path $exe)     'artifacts: tbg-lite.exe built (inspect helper for AUMID reads)'
  Assert (Test-Path $exeInj)  'artifacts: tbg-inject.exe built'
  Assert (Test-Path $dllHook) 'artifacts: tbg_hook.dll built'

  # ---------------- CI0: environment probe ----------------
  Log '=== CI0: environment probe (classic taskbar leg) ==='
  $os = Get-CimInstance Win32_OperatingSystem
  Log ("OS: {0} (version {1})" -f $os.Caption, $os.Version)
  $np = Get-Command notepad.exe -ErrorAction SilentlyContinue
  $mp = Get-Command mspaint.exe -ErrorAction SilentlyContinue
  Log ("notepad: {0} | mspaint: {1}" -f ($np -ne $null), ($mp -ne $null))
  $expl0 = Get-Process -Name explorer -ErrorAction SilentlyContinue | Select-Object -First 1
  Assert ($null -ne $expl0) 'CI0: explorer is running (interactive shell present)'
  Log ("explorer pid: {0}" -f $expl0.Id)

  # ---------------- CI1: baseline (native grouping reference) ----------------
  Log '=== CI1: baseline -- native grouping on the classic taskbar ==='
  Clear-TestWindows
  Start-Sleep -Seconds 2
  $live = Spawn-Notepads 2
  $baseNp = Wait-ButtonCount '*Notepad*' 1 20
  Log ("baseline notepad buttons: {0} (1 = classic default combine; 2 = no native combining, line-1 becomes vacuous)" -f $baseNp)
  Assert ($baseNp -ge 1) "CI1: baseline notepad button count sane (got $baseNp)"
  $a0 = Get-WindowAumid $live[0].Hwnd
  Log ("baseline notepad AUMID: [{0}]" -f $a0)
  Shot 'inj-classic-baseline.png'
  Clear-TestWindows
  Start-Sleep -Seconds 2

  # ---------------- CI2: line-1 inject (ungroup) ----------------
  Log '=== CI2: line-1 inject -- ungroup on the classic taskbar ==='
  $inj1 = & $exeInj inject --strategy ungroup | Out-String
  Log "inject output >>> $inj1"
  Assert (($LASTEXITCODE -eq 0) -and ($inj1 -cmatch 'inject: ok')) 'CI2: inject --strategy ungroup exits 0'
  Start-Sleep -Seconds 2
  $live = Spawn-Notepads 2
  Start-Sleep -Seconds 5
  $injNp = Wait-ButtonCount '*Notepad*' 2 20
  Log ("injected notepad buttons: {0}" -f $injNp)
  Shot 'inj-classic-ungrouped.png'
  $st = & $exeInj status | Out-String
  $stflat = $st -replace "`r|`n", ' '
  Log "status while active: $stflat"
  Assert ($st -cmatch 'state\s*:\s*active') 'CI2: status reports active'
  $callsOk = $false
  if ($st -match 'calls=(\d+)') { $callsOk = ([int]$Matches[1] -ge 1) }
  Assert $callsOk 'CI2: calls >= 1 (taskbar queried through the hook)'
  $wrappedOk = $false
  if ($st -match 'wrapped=(\d+)') { $wrappedOk = ([int]$Matches[1] -ge 1) }
  Assert $wrappedOk 'CI2: wrapped >= 1 (stores handed to the taskbar as proxies)'
  $servedOk = $false
  if ($st -match 'aumid-served=(\d+)') { $servedOk = ([int]$Matches[1] -ge 1) }
  Assert $servedOk 'CI2: aumid-served >= 1 (classic taskbar READS PKEY_AppUserModel_ID through the hook -- route A decisive proof)'
  $a1 = Get-WindowAumid $live[0].Hwnd
  Log ("real notepad AUMID while injected: [{0}]" -f $a1)
  Assert ($a1 -notmatch '~TBG~w') 'CI2: real window AUMID untouched while injected (read-path rewrite only)'
  Assert (Test-HookDllLoaded) 'CI2: tbg_hook.dll present in explorer module list while injected (enumeration sanity)'
  if ($baseNp -eq 1) {
    Assert ($injNp -eq 2) "CI2: injected notepads get TWO separate buttons (route A works on the classic taskbar; got $injNp)"
  } else {
    Log "CI2: baseline showed no native combining ($baseNp buttons); two-button assertion skipped, served-counter is the proof"
  }
  # stop -> native grouping returns
  $sp1 = & $exeInj stop | Out-String
  Assert (($LASTEXITCODE -eq 0) -and ($sp1 -cmatch 'stop: ok')) 'CI2: stop exits 0 with "stop: ok"'
  Assert (Wait-HookDllGone 10) 'CI2: tbg_hook.dll really left the explorer module list after stop (fix round 9 guard)'
  Clear-TestWindows
  Start-Sleep -Seconds 2
  $live = Spawn-Notepads 2
  Start-Sleep -Seconds 4
  $postNp = Wait-ButtonCount '*Notepad*' $baseNp 20
  Assert ($postNp -eq $baseNp) "CI2: native grouping returns after stop (got $postNp, baseline $baseNp)"
  Clear-TestWindows
  Start-Sleep -Seconds 2

  # ---------------- CI3: line-2 inject (group, cross-app) ----------------
  Log '=== CI3: line-2 inject -- group across two apps ==='
  $live2 = Spawn-Notepads 1
  $paint = Spawn-Mspaint
  Start-Sleep -Seconds 4
  $btnsB = Get-TaskbarButtonNames
  $bNp = @($btnsB | Where-Object { $_ -like '*Notepad*' }).Count
  $bPt = @($btnsB | Where-Object { $_ -like '*Paint*' -or $_ -like '*paint*' }).Count
  $baseTotal = $bNp + $bPt
  Log ("cross-app baseline: notepad buttons={0}, paint buttons={1}, total={2}" -f $bNp, $bPt, $baseTotal)
  Shot 'inj-classic-crossapp-baseline.png'
  Clear-TestWindows
  Start-Sleep -Seconds 2
  if ($baseTotal -eq 2) {
    $inj2 = & $exeInj inject --strategy group --group cls | Out-String
    Log "inject output >>> $inj2"
    Assert (($LASTEXITCODE -eq 0) -and ($inj2 -cmatch 'inject: ok')) 'CI3: inject --strategy group exits 0'
    Start-Sleep -Seconds 2
    $live3 = Spawn-Notepads 1
    $paint2 = Spawn-Mspaint
    Start-Sleep -Seconds 5
    $btnsG = Get-TaskbarButtonNames
    $gNp = @($btnsG | Where-Object { $_ -like '*Notepad*' }).Count
    $gPt = @($btnsG | Where-Object { $_ -like '*Paint*' -or $_ -like '*paint*' }).Count
    Log ("cross-app grouped: notepad buttons={0}, paint buttons={1}" -f $gNp, $gPt)
    Shot 'inj-classic-crossapp-grouped.png'
    Assert (($gNp + $gPt) -eq 1) "CI3: notepad + mspaint merge into ONE button under the shared AUMID (got $($gNp + $gPt))"
    $st3 = & $exeInj status | Out-String
    Log ("status: " + ($st3 -replace "`r|`n", ' '))
    $sp3 = & $exeInj stop | Out-String
    Assert (($LASTEXITCODE -eq 0) -and ($sp3 -cmatch 'stop: ok')) 'CI3: stop exits 0'
  } else {
    Log "CI3: cross-app baseline was $baseTotal (expected 2); grouping assertion skipped, logged for diagnosis"
  }
  Clear-TestWindows
  Start-Sleep -Seconds 2

  # ---------------- CI4: explorer restart ----------------
  Log '=== CI4: explorer restart -- hook dies, re-inject works ==='
  $newPid = Restart-ExplorerShell
  Assert ($newPid -ne 0) 'CI4: explorer restarted with a new pid'
  Start-Sleep -Seconds 3
  $st4 = & $exeInj status | Out-String
  Assert ($st4 -cmatch 'state\s*:\s*detached') 'CI4: status is detached after the restart (hook died with the shell)'
  $inj4 = & $exeInj inject | Out-String
  Assert (($LASTEXITCODE -eq 0) -and ($inj4 -cmatch 'inject: ok')) 'CI4: re-inject into the new explorer succeeds'
  $sp4 = & $exeInj stop | Out-String
  Assert (($LASTEXITCODE -eq 0) -and ($sp4 -cmatch 'stop: ok')) 'CI4: stop after re-inject exits 0'
  Assert (Wait-HookDllGone 10) 'CI4: tbg_hook.dll left explorer after the final stop (early-injection instance unloads cleanly)'
} catch {
  Fail ("unexpected exception: " + $_.Exception.Message)
  Log ($_.ScriptStackTrace -replace "`r|`n", ' | ')
} finally {
  # best-effort cleanup so the runner stays usable for artifact upload
  try { & $exeInj stop *> $null } catch { }
  Clear-TestWindows
}

# ---------------- result ----------------
$result = New-Object System.Collections.Generic.List[string]
$result += "suite   : runtime-smoke-inj (classic taskbar leg, windows-2022)"
$result += "passed  : $($passes.Count)"
$result += "failed  : $($failures.Count)"
if ($failures.Count -gt 0) { $result += $failures }
$result | Set-Content (Join-Path $out 'RESULT-INJ.txt')
Log "RESULT: $($passes.Count) passed, $($failures.Count) failed (details in ci/out/RESULT-INJ.txt)"
if ($failures.Count -gt 0) { exit 1 }
exit 0
