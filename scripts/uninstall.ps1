<#
.SYNOPSIS
    Removes the Screen Control service. Run as Administrator.

.DESCRIPTION
    Stops and deletes the service, removes the Safe Mode keys and the program.
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

function Step($text) { Write-Host "==> $text" -ForegroundColor Cyan }

$service = Get-Service $ServiceName -ErrorAction SilentlyContinue
if ($service) {
    # A normal stop is not a failure, so the restart-on-failure actions do not run.
    if ($service.Status -ne "Stopped") {
        Step "Stopping the service"
        Stop-Service $ServiceName -Force
        (Get-Service $ServiceName).WaitForStatus("Stopped", (New-TimeSpan -Seconds 30))
    }
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
    Remove-Item $DataDir -Recurse -Force -ErrorAction SilentlyContinue
} else {
    Write-Host "The data folder $DataDir was kept. Use -RemoveData to delete it."
}

Write-Host "Screen Control is removed." -ForegroundColor Green
