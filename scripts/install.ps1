<#
.SYNOPSIS
    Installs or updates the Screen Control service. Run as Administrator.

.DESCRIPTION
    Copies the program to Program Files, creates the data folder that only SYSTEM and
    Administrators can open, writes secrets.json, gets the config, and creates the service
    with restart on failure and Safe Mode support.

    Running it again updates the program. Saved state, secrets and config are kept
    unless new values are given.

.EXAMPLE
    .\install.ps1 -ConfigFile .\pc-1.json
    Install with a local config file. Asks for the secrets.

.EXAMPLE
    .\install.ps1 -GitHubRepo owner/screen-control-config
    Install and download pcs\<PC name>.json from the config repo.

.EXAMPLE
    .\install.ps1 -ExePath .\screen-control.exe
    Update the program only.

.EXAMPLE
    .\install.ps1 -CheckTelegram
    Install nothing. Show the bot link and send a test message to each parent again.
#>
#Requires -RunAsAdministrator
[CmdletBinding()]
param(
    # The program to install. By default, next to this script or in ..\target\release.
    [string]$ExePath,
    # A local config file to use, instead of downloading it.
    [string]$ConfigFile,
    # The name of this PC in the config. By default, the Windows computer name.
    [string]$PcName,
    [string]$BotToken,
    [string]$GitHubToken,
    # The config repo, as owner/repo.
    [string]$GitHubRepo,
    # Only show the bot link and send a test message to each parent. Installs nothing.
    [switch]$CheckTelegram
)

$ErrorActionPreference = "Stop"

$ServiceName = "ScreenControl"
$InstallDir = Join-Path $env:ProgramFiles "ScreenControl"
$DataDir = Join-Path $env:ProgramData "ScreenTime"
$InstalledExe = Join-Path $InstallDir "screen-control.exe"
$SecretsPath = Join-Path $DataDir "secrets.json"
$ConfigPath = Join-Path $DataDir "config.json"
$SafeBootKeys = @(
    "HKLM:\SYSTEM\CurrentControlSet\Control\SafeBoot\Minimal\$ServiceName",
    "HKLM:\SYSTEM\CurrentControlSet\Control\SafeBoot\Network\$ServiceName"
)

function Step($text) { Write-Host "==> $text" -ForegroundColor Cyan }

# JSON without a byte order mark. Windows PowerShell 5.1 adds one with -Encoding UTF8.
function Write-Utf8File($path, $text) {
    [IO.File]::WriteAllText($path, $text, (New-Object Text.UTF8Encoding($false)))
}

function Read-Value($prompt, $current) {
    $hint = if ($current) { " (Enter keeps the current value)" } else { " (Enter to leave empty)" }
    $value = Read-Host "$prompt$hint"
    if ($value) { $value } else { $current }
}

# --- Telegram check ----------------------------------------------------------------------
# A bot can only send messages to a user who pressed Start in the chat with the bot.
# These checks only print warnings. They never stop the install.
# The bot token is in each request URL, so error messages from Invoke-RestMethod are
# never printed. Only the HTTP status and the Telegram description are shown.

$TelegramApi = "https://api.telegram.org"

function Warn($text) { Write-Host "    $text" -ForegroundColor Yellow }

# Calls a Bot API method. Never throws. Returns an object with:
# ok, result, code (HTTP status, 0 = no answer), description.
function Invoke-Telegram($token, $method, $body) {
    $answer = [pscustomobject]@{ ok = $false; result = $null; code = 0; description = "" }
    try {
        [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12
        $params = @{ Uri = "$TelegramApi/bot$token/$method"; Method = "Post"; TimeoutSec = 15; UseBasicParsing = $true }
        if ($body) {
            $params.ContentType = "application/json; charset=utf-8"
            $params.Body = [Text.Encoding]::UTF8.GetBytes(($body | ConvertTo-Json -Compress))
        }
        $reply = Invoke-RestMethod @params
        $answer.ok = [bool]$reply.ok
        $answer.result = $reply.result
        $answer.code = 200
        $answer.description = [string]$reply.description
    } catch {
        $response = $null
        if ($_.Exception.Response) { $response = $_.Exception.Response }
        if ($response) {
            try { $answer.code = [int]$response.StatusCode } catch { }
            # The body of the error answer: {"ok":false,"error_code":403,"description":"..."}
            $text = $null
            if ($_.ErrorDetails -and $_.ErrorDetails.Message) {
                $text = $_.ErrorDetails.Message
            } else {
                try {
                    $reader = New-Object IO.StreamReader($response.GetResponseStream())
                    $text = $reader.ReadToEnd()
                } catch { }
            }
            try { $answer.description = [string]($text | ConvertFrom-Json).description } catch { }
        }
    }
    $answer
}

function Test-Telegram($token, $configPath) {
    Write-Host ""
    Step "Telegram check"
    if (-not $token) {
        Warn "No bot token in $SecretsPath. Telegram check skipped."
        return
    }
    try {
        $config = [IO.File]::ReadAllText($configPath) | ConvertFrom-Json
    } catch {
        Warn "Cannot read the config $configPath. Telegram check skipped."
        return
    }

    $me = Invoke-Telegram $token "getMe" $null
    if ($me.code -eq 0) {
        Warn "Cannot reach Telegram (no network?). Telegram check skipped."
        Warn "Re-check later: $RecheckCommand"
        return
    }
    if (-not $me.ok) {
        if ($me.code -eq 401 -or $me.code -eq 404) {
            Warn "The bot token is wrong (HTTP $($me.code)). Install again with the right -BotToken."
        } else {
            Warn "getMe failed: HTTP $($me.code) $($me.description)"
        }
        return
    }
    $link = "https://t.me/$($me.result.username)"
    Write-Host "    Bot: @$($me.result.username)"
    Write-Host "    Link: $link"
    Write-Host "    Each parent (and child with a Telegram ID) must open this link and press Start."

    $text = "Screen Control is installed on $($config.pc_name)."
    $missing = @()
    foreach ($parent in @($config.parents)) {
        # Telegram user IDs fit in Int64. Send them as text so no digits are lost.
        $id = ([int64]$parent.telegram_user_id).ToString([Globalization.CultureInfo]::InvariantCulture)
        $sent = Invoke-Telegram $token "sendMessage" @{ chat_id = $id; text = $text }
        if ($sent.ok) {
            Write-Host "    $($parent.name) ($id): OK" -ForegroundColor Green
        } elseif ($sent.code -eq 0) {
            Warn "$($parent.name) ($id): no answer from Telegram (no network?)"
            $missing += $parent.name
        } elseif ($sent.code -eq 403 -or ($sent.code -eq 400 -and $sent.description -match "chat not found")) {
            Warn "$($parent.name) ($id): not reached - has not pressed Start yet (or wrong ID)"
            $missing += $parent.name
        } else {
            Warn "$($parent.name) ($id): not reached - HTTP $($sent.code) $($sent.description)"
            $missing += $parent.name
        }
    }

    if ($missing.Count -eq 0) {
        Write-Host "    All parents got the test message." -ForegroundColor Green
    } else {
        Warn "Not reached: $($missing -join ', ')."
        Warn "They must open $link and press Start. Then re-check:"
        Warn "    $RecheckCommand"
    }
}

$RecheckCommand = "powershell -ExecutionPolicy Bypass -File `"$PSCommandPath`" -CheckTelegram   (as Administrator)"

if ($CheckTelegram) {
    try {
        $token = (Get-Content $SecretsPath -Raw | ConvertFrom-Json).bot_token
        Test-Telegram $token $ConfigPath
    } catch {
        Warn "Cannot read $SecretsPath. Is Screen Control installed?"
    }
    exit 0
}

# --- Find the program ---------------------------------------------------------------

if (-not $ExePath) {
    $candidates = @(
        (Join-Path $PSScriptRoot "screen-control.exe"),
        (Join-Path $PSScriptRoot "..\target\release\screen-control.exe")
    )
    $ExePath = $candidates | Where-Object { Test-Path $_ } | Select-Object -First 1
    if (-not $ExePath) { throw "screen-control.exe not found. Give it with -ExePath." }
}
$ExePath = (Resolve-Path $ExePath).Path
Step "Program: $ExePath"

# --- Data folder: only SYSTEM and Administrators ----------------------------------------

Step "Data folder $DataDir"
New-Item -ItemType Directory -Force $DataDir | Out-Null
# SIDs, so this works in every Windows language. S-1-5-18 = SYSTEM, S-1-5-32-544 = Administrators.
icacls $DataDir /inheritance:r /grant:r "*S-1-5-18:(OI)(CI)F" "*S-1-5-32-544:(OI)(CI)F" | Out-Null
if ($LASTEXITCODE -ne 0) { throw "icacls failed on $DataDir" }
# Files already in the folder take the folder's rights.
if (Get-ChildItem $DataDir) {
    icacls "$DataDir\*" /reset /T /Q | Out-Null
    if ($LASTEXITCODE -ne 0) { throw "icacls failed on the files in $DataDir" }
}

# --- secrets.json ------------------------------------------------------------------------

$secrets = [ordered]@{ pc_name = $env:COMPUTERNAME; bot_token = $null; github_token = $null; github_repo = $null }
if (Test-Path $SecretsPath) {
    $old = Get-Content $SecretsPath -Raw | ConvertFrom-Json
    foreach ($key in @($secrets.Keys)) {
        if ($old.$key) { $secrets[$key] = $old.$key }
    }
}
$given = $PcName -or $BotToken -or $GitHubToken -or $GitHubRepo
if ($PcName) { $secrets.pc_name = $PcName }
if ($BotToken) { $secrets.bot_token = $BotToken }
if ($GitHubToken) { $secrets.github_token = $GitHubToken }
if ($GitHubRepo) { $secrets.github_repo = $GitHubRepo }
if (-not (Test-Path $SecretsPath) -and -not $given) {
    Step "Secrets (they are stored only on this PC)"
    $secrets.pc_name = Read-Value "PC name, as in the config" $secrets.pc_name
    $secrets.bot_token = Read-Value "Telegram bot token" $secrets.bot_token
    $secrets.github_repo = Read-Value "Config repo (owner/repo)" $secrets.github_repo
    $secrets.github_token = Read-Value "GitHub read-only token" $secrets.github_token
}
Write-Utf8File $SecretsPath ($secrets | ConvertTo-Json)
Step "Secrets saved for PC `"$($secrets.pc_name)`""

# --- config.json -------------------------------------------------------------------------

$newConfig = Join-Path $DataDir "config.new.json"
if ($ConfigFile) {
    Step "Config from $ConfigFile"
    Copy-Item $ConfigFile $newConfig -Force
} elseif ($secrets.github_repo -and $secrets.github_token) {
    $url = "https://api.github.com/repos/$($secrets.github_repo)/contents/pcs/$($secrets.pc_name).json"
    Step "Config from $url"
    $headers = @{
        Authorization = "Bearer $($secrets.github_token)"
        Accept = "application/vnd.github.raw+json"
        "X-GitHub-Api-Version" = "2022-11-28"
    }
    Invoke-WebRequest $url -Headers $headers -OutFile $newConfig -UseBasicParsing
} elseif (Test-Path $ConfigPath) {
    Step "Keeping the current config"
} else {
    throw "No config. Give -ConfigFile, or a config repo and GitHub token."
}

if (Test-Path $newConfig) {
    # Check the config with the new program before anything is changed.
    & $ExePath check-config $newConfig --pc-name $secrets.pc_name
    if ($LASTEXITCODE -ne 0) {
        Remove-Item $newConfig
        throw "The config has errors. Nothing was installed."
    }
    Move-Item $newConfig $ConfigPath -Force
}

# --- Program and service -----------------------------------------------------------------

$service = Get-Service $ServiceName -ErrorAction SilentlyContinue
if ($service -and $service.Status -ne "Stopped") {
    Step "Stopping the service"
    Stop-Service $ServiceName -Force
    (Get-Service $ServiceName).WaitForStatus("Stopped", (New-TimeSpan -Seconds 30))
}

Step "Copying the program to $InstallDir"
New-Item -ItemType Directory -Force $InstallDir | Out-Null
Copy-Item $ExePath $InstalledExe -Force

$binPath = "`"$InstalledExe`" service"
if ($service) {
    sc.exe config $ServiceName binPath= $binPath start= auto | Out-Null
} else {
    Step "Creating the service"
    New-Service -Name $ServiceName -DisplayName "Screen Control" -StartupType Automatic `
        -BinaryPathName $binPath -Description "Limits daily screen time for children." | Out-Null
}
# Restart 5 s after any failure, also when the service stops with an error code.
sc.exe failure $ServiceName reset= 86400 actions= restart/5000/restart/5000/restart/5000 | Out-Null
sc.exe failureflag $ServiceName 1 | Out-Null

Step "Safe Mode support"
foreach ($key in $SafeBootKeys) {
    New-Item -Path $key -Force | Out-Null
    Set-ItemProperty -Path $key -Name "(default)" -Value "Service"
}

Step "Starting the service"
Start-Service $ServiceName
Start-Sleep -Seconds 3
$status = (Get-Service $ServiceName).Status
if ($status -ne "Running") { throw "The service is $status. See the log in $DataDir\logs." }

Write-Host ""
Write-Host "Screen Control is installed and running." -ForegroundColor Green
Write-Host "Last log lines:"
Get-ChildItem "$DataDir\logs\*.log" | Sort-Object LastWriteTime | Select-Object -Last 1 |
    ForEach-Object { Get-Content $_.FullName -Tail 5 }

# Telegram problems only print warnings. The service is already installed.
try { Test-Telegram $secrets.bot_token $ConfigPath } catch { Warn "Telegram check failed. Re-check later: $RecheckCommand" }
