# install-task.ps1

# - 로그온 시 rust-agents-proxy serve 상주 실행 작업 스케줄러 등록·해제·상태 확인
# - 콘솔 창 없이 실행 (conhost --headless), 실패 시 1분 간격 재시작
# - 비대화형 실행: -Action Install|Uninstall|Status -ExePath <exe> -Yes

param(
    [ValidateSet("", "Install", "Uninstall", "Status")]
    [string] $Action = "",
    [string] $ExePath = "",
    [switch] $Yes
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$script:TaskName = "rust-agents-proxy"
$script:DefaultExe = Join-Path (Split-Path -Parent $PSScriptRoot) "target\release\rust-agents-proxy.exe"
$script:Interactive = [Environment]::UserInteractive -and -not [Console]::IsInputRedirected -and
    -not ([Environment]::GetCommandLineArgs() | Where-Object { $_ -match '^-noni' })

# 1. 작업 선택 -----------------------------------------------------------------
& {
    if ($Action -ne "") {
        return
    }
    if (-not $script:Interactive) {
        $script:Action = "Status"
        return
    }
    $choices = @(
        [System.Management.Automation.Host.ChoiceDescription]::new("&Install", "로그온 시 serve 상주 등록")
        [System.Management.Automation.Host.ChoiceDescription]::new("&Uninstall", "등록 해제")
        [System.Management.Automation.Host.ChoiceDescription]::new("&Status", "등록 상태 확인")
    )
    $picked = $Host.UI.PromptForChoice("rust-agents-proxy", "작업 선택", $choices, 2)
    $script:Action = @("Install", "Uninstall", "Status")[$picked]
}

# 2. 상태 확인 -----------------------------------------------------------------
$existing = Get-ScheduledTask -TaskName $script:TaskName -ErrorAction SilentlyContinue
if ($Action -eq "Status") {
    if ($null -eq $existing) {
        Write-Host "Task not registered: $($script:TaskName)"
        exit 0
    }
    $taskInfo = Get-ScheduledTaskInfo -TaskName $script:TaskName
    Write-Host "Task: $($script:TaskName) [$($existing.State)] last run $($taskInfo.LastRunTime) result $($taskInfo.LastTaskResult)"
    exit 0
}

# 3. 등록 해제 -----------------------------------------------------------------
if ($Action -eq "Uninstall") {
    if ($null -eq $existing) {
        Write-Host "Task not registered: $($script:TaskName)"
        exit 0
    }
    if (-not $Yes -and $script:Interactive) {
        $confirm = $Host.UI.PromptForChoice("rust-agents-proxy", "작업 해제 진행", @("&Yes", "&No"), 1)
        if ($confirm -ne 0) {
            Write-Host "Cancelled."
            exit 0
        }
    }
    Unregister-ScheduledTask -TaskName $script:TaskName -Confirm:$false
    Write-Host "Unregistered: $($script:TaskName)"
    exit 0
}

# 4. 실행 파일 경로 ------------------------------------------------------------
if ($ExePath -eq "") {
    $ExePath = $script:DefaultExe
    if ($script:Interactive) {
        $typed = Read-Host "Exe path [$($script:DefaultExe)]"
        if ($typed.Trim() -ne "") {
            $ExePath = $typed.Trim()
        }
    }
}
if (-not (Test-Path -LiteralPath $ExePath -PathType Leaf)) {
    [Console]::Error.WriteLine("Exe not found: $ExePath (run .cmd\build.cmd first)")
    exit 1
}
$ExePath = (Resolve-Path -LiteralPath $ExePath).Path

# 5. 등록 요약·확인 ------------------------------------------------------------
Write-Host "Task: $($script:TaskName)"
Write-Host "Exe:  $ExePath"
Write-Host "Run:  conhost.exe --headless `"$ExePath`" serve (at logon, restart on failure)"
if (-not $Yes -and $script:Interactive) {
    $confirm = $Host.UI.PromptForChoice("rust-agents-proxy", "등록 진행", @("&Yes", "&No"), 0)
    if ($confirm -ne 0) {
        Write-Host "Cancelled."
        exit 0
    }
}

# 6. 작업 등록 -----------------------------------------------------------------
$userId = [System.Security.Principal.WindowsIdentity]::GetCurrent().Name
$taskAction = New-ScheduledTaskAction -Execute "conhost.exe" -Argument "--headless `"$ExePath`" serve" -WorkingDirectory (Split-Path -Parent $ExePath)
$taskTrigger = New-ScheduledTaskTrigger -AtLogOn -User $userId
$taskPrincipal = New-ScheduledTaskPrincipal -UserId $userId -LogonType Interactive -RunLevel Limited
$taskSettings = New-ScheduledTaskSettingsSet -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries `
    -ExecutionTimeLimit ([TimeSpan]::Zero) -RestartCount 999 -RestartInterval (New-TimeSpan -Minutes 1) `
    -MultipleInstances IgnoreNew -StartWhenAvailable
Register-ScheduledTask -TaskName $script:TaskName -Action $taskAction -Trigger $taskTrigger `
    -Principal $taskPrincipal -Settings $taskSettings -Force | Out-Null
Write-Host "Registered: $($script:TaskName) (starts at next logon; run Start-ScheduledTask -TaskName $($script:TaskName) to start now)"
exit 0
