<#
.SYNOPSIS
  Keeps a checkout on top of its upstream: fetches, fast-forwards the local
  main, and rebases the checked-out branch onto upstream main. Never pushes.

.DESCRIPTION
  Meant to run every few minutes from Task Scheduler (see -Register). Each
  run does nothing and says why when:
    - the working tree has changes to tracked files,
    - a rebase, merge, cherry-pick or revert is in progress,
    - HEAD is detached,
    - cargo, rustc or victory is running (a rebase would change the source
      under a build or a run).
  A rebase that stops on a conflict is aborted, so the branch is left as it
  was, and the conflict is logged; it is tried again next time (and logged
  again only when upstream has moved).

  Log: %LOCALAPPDATA%\victory-watchdog\<repo folder name>.log

.EXAMPLE
  powershell -File scripts\upstream-watchdog.ps1 -Register
  powershell -File scripts\upstream-watchdog.ps1 -Unregister
  powershell -File scripts\upstream-watchdog.ps1          # one run now
#>
param(
    [string]$Repo = (Split-Path -Parent $PSScriptRoot),
    [string]$Remote = 'origin',
    [string]$UpstreamBranch = 'main',
    [int]$Minutes = 10,
    [switch]$Register,
    [switch]$Unregister
)

$ErrorActionPreference = 'Continue'
$Repo = (Resolve-Path $Repo).Path
$name = Split-Path -Leaf $Repo
$taskName = "VIctory upstream rebase ($name)"

if ($Register) {
    $script = $MyInvocation.MyCommand.Path
    # conhost --headless: no console window at all (powershell's own
    # -WindowStyle Hidden still flashes one for a moment every run).
    $action = New-ScheduledTaskAction -Execute 'conhost.exe' -Argument (
        "--headless powershell.exe -NoProfile -NonInteractive -WindowStyle Hidden -ExecutionPolicy Bypass " +
        "-File `"$script`" -Repo `"$Repo`" -Remote $Remote -UpstreamBranch $UpstreamBranch")
    $trigger = New-ScheduledTaskTrigger -Once -At (Get-Date).AddMinutes(1) `
        -RepetitionInterval (New-TimeSpan -Minutes $Minutes)
    $settings = New-ScheduledTaskSettingsSet -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries `
        -StartWhenAvailable -MultipleInstances IgnoreNew -ExecutionTimeLimit (New-TimeSpan -Minutes 5)
    Register-ScheduledTask -TaskName $taskName -Action $action -Trigger $trigger -Settings $settings `
        -Description "Fetch $Remote, fast-forward $UpstreamBranch, rebase the checked-out branch of $Repo every $Minutes minutes. Never pushes." `
        -Force | Out-Null
    Write-Output "Registered '$taskName' (every $Minutes min)."
    return
}
if ($Unregister) {
    Unregister-ScheduledTask -TaskName $taskName -Confirm:$false
    Write-Output "Removed '$taskName'."
    return
}

$logDir = Join-Path $env:LOCALAPPDATA 'victory-watchdog'
New-Item -ItemType Directory -Force $logDir | Out-Null
$log = Join-Path $logDir "$name.log"
$state = Join-Path $logDir "$name.last-conflict"
function Write-Log([string]$msg) {
    "{0:yyyy-MM-dd HH:mm:ss} {1}" -f (Get-Date), $msg | Out-File -Append -Encoding utf8 $log
}
function Invoke-Git {
    $out = & git -C $Repo @args 2>&1 | ForEach-Object { "$_" }
    [pscustomobject]@{ Ok = ($LASTEXITCODE -eq 0); Out = ($out -join "`n") }
}

# Keep the log short.
if ((Test-Path $log) -and (Get-Item $log).Length -gt 1MB) {
    Get-Content $log -Tail 2000 | Set-Content -Encoding utf8 "$log.tmp"
    Move-Item -Force "$log.tmp" $log
}

$busy = Get-Process -Name cargo, rustc, victory -ErrorAction SilentlyContinue
if ($busy) { Write-Log "skip: $(($busy.Name | Sort-Object -Unique) -join ', ') running"; return }

$gitDir = (Invoke-Git rev-parse --absolute-git-dir).Out
foreach ($m in 'rebase-merge', 'rebase-apply', 'MERGE_HEAD', 'CHERRY_PICK_HEAD', 'REVERT_HEAD') {
    if (Test-Path (Join-Path $gitDir $m)) { Write-Log "skip: $m in progress"; return }
}
$dirty = (Invoke-Git status --porcelain --untracked-files=no).Out
if ($dirty) { Write-Log 'skip: uncommitted changes to tracked files'; return }

$branch = Invoke-Git symbolic-ref --quiet --short HEAD
if (-not $branch.Ok) { Write-Log 'skip: detached HEAD'; return }
$branch = $branch.Out

$fetch = Invoke-Git fetch --quiet --prune $Remote
if (-not $fetch.Ok) { Write-Log "fetch failed: $($fetch.Out)"; return }
$up = "$Remote/$UpstreamBranch"
$upSha = (Invoke-Git rev-parse --short $up).Out

# The local copy of upstream main, fast-forward only.
if ($branch -ne $UpstreamBranch) {
    $ff = Invoke-Git fetch --quiet . "${up}:$UpstreamBranch"
    if (-not $ff.Ok) { Write-Log "local $UpstreamBranch not fast-forwarded to $up ($upSha): it has commits of its own" }
}

if ((Invoke-Git merge-base --is-ancestor $up HEAD).Ok) { return }  # already on top: say nothing

if ($branch -eq $UpstreamBranch) {
    $r = Invoke-Git merge --ff-only --quiet $up
    if ($r.Ok) { Write-Log "$branch fast-forwarded to $upSha" }
    else { Write-Log "$branch not fast-forwarded to ${upSha}: $($r.Out)" }
    return
}

$before = (Invoke-Git rev-parse --short HEAD).Out
$r = Invoke-Git -c core.editor=true rebase --quiet $up
if ($r.Ok) {
    $after = (Invoke-Git rev-parse --short HEAD).Out
    Write-Log "$branch rebased onto $up ($upSha): $before -> $after"
    Remove-Item $state -ErrorAction SilentlyContinue
    return
}
$files = (Invoke-Git diff --name-only --diff-filter=U).Out -replace "`n", ', '
Invoke-Git rebase --abort | Out-Null
$last = if (Test-Path $state) { Get-Content $state -Raw } else { '' }
if ($last.Trim() -ne $upSha) {
    Write-Log "CONFLICT rebasing $branch onto $up ($upSha) in: $files. Rebase aborted; $branch left at $before."
    Set-Content -Encoding utf8 $state $upSha
}
