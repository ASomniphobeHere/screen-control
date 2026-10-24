<#
.SYNOPSIS
    Removes the Screen Control service. Run as Administrator.

.DESCRIPTION
    Stops and deletes the service, ends the helpers in the children's sessions, and removes
    the Safe Mode keys and the program.
    The data folder (config, secrets, state, logs) is kept unless -RemoveData is given.
#>
#Requires -RunAsAdministrator
[CmdletBinding()]
param(
    # Also delete C:\ProgramData\ScreenTime with the config, secrets, state and logs.
    [switch]$RemoveData
)

$ErrorActionPreference = "Stop"

$ServiceName = "ScreenControl"
$InstallDir = Join-Path $env:ProgramFiles "ScreenControl"
$DataDir = Join-Path $env:ProgramData "ScreenTime"
$InstalledExe = Join-Path $InstallDir "screen-control.exe"

function Step($text) { Write-Host "==> $text" -ForegroundColor Cyan }

# Stops the service and waits until its process has ended, then ends the helpers in the
# children's sessions. Windows shows the service as Stopped a little before its process ends,
# and the helpers keep running without the service. Both keep files open: the program in
# $InstallDir and the logs in $DataDir.
function Stop-ScreenControl {
    $service = Get-CimInstance Win32_Service -Filter "Name='$ServiceName'"
    if ($service -and $service.State -ne "Stopped") {
        Step "Stopping the service"
        Stop-Service $ServiceName -Force
        (Get-Service $ServiceName).WaitForStatus("Stopped", (New-TimeSpan -Seconds 30))
        if ($service.ProcessId) {
            Wait-Process -Id $service.ProcessId -Timeout 15 -ErrorAction SilentlyContinue
            Stop-Process -Id $service.ProcessId -Force -ErrorAction SilentlyContinue
        }
    }
    $helpers = @(Get-CimInstance Win32_Process -Filter "Name='screen-control.exe'" |
        Where-Object { $_.ExecutablePath -eq $InstalledExe })
    if ($helpers) {
        Step "Ending $($helpers.Count) helper(s)"
        foreach ($helper in $helpers) {
            Stop-Process -Id $helper.ProcessId -Force -ErrorAction SilentlyContinue
        }
        Wait-Process -Id $helpers.ProcessId -Timeout 15 -ErrorAction SilentlyContinue
    }
}

# A normal stop is not a failure, so the restart-on-failure actions do not run.
# Also ends helpers left from an earlier uninstall that failed.
Stop-ScreenControl
if (Get-Service $ServiceName -ErrorAction SilentlyContinue) {
    Step "Deleting the service"
    sc.exe delete $ServiceName | Out-Null
} else {
    Step "No service to delete"
}

Step "Removing Safe Mode keys"
foreach ($mode in "Minimal", "Network") {
    Remove-Item "HKLM:\SYSTEM\CurrentControlSet\Control\SafeBoot\$mode\$ServiceName" -ErrorAction SilentlyContinue
}

if (Test-Path $InstallDir) {
    Step "Removing $InstallDir"
    Remove-Item $InstallDir -Recurse -Force
}

if ($RemoveData) {
    Step "Removing $DataDir"
    Remove-Item $DataDir -Recurse -Force
} else {
    Write-Host "The data folder $DataDir was kept. Use -RemoveData to delete it."
}

Write-Host "Screen Control is removed." -ForegroundColor Green
