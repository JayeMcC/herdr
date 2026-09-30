# managed by herdr; reinstalling the integration replaces this file.
# TWODR_INTEGRATION_ID=letta
# TWODR_INTEGRATION_VERSION=1

param([string]$Action = "")
# twodr panes export TWODR_* and, until it is dropped, a HERDR_* alias.
# Prefer the twodr name so a pane from either side of that change reports.
if ($env:TWODR_ENV) { $env:HERDR_ENV = $env:TWODR_ENV }
if ($env:TWODR_PANE_ID) { $env:HERDR_PANE_ID = $env:TWODR_PANE_ID }
if ($env:TWODR_SOCKET_PATH) { $env:HERDR_SOCKET_PATH = $env:TWODR_SOCKET_PATH }
if ($env:TWODR_BIN_PATH) { $env:HERDR_BIN_PATH = $env:TWODR_BIN_PATH }

if ($Action -ne "session") { exit 0 }
if ($env:HERDR_ENV -ne "1") { exit 0 }
if ([string]::IsNullOrWhiteSpace($env:HERDR_PANE_ID)) { exit 0 }
if ([string]::IsNullOrWhiteSpace($env:HERDR_SOCKET_PATH)) { exit 0 }

$inputText = [Console]::In.ReadToEnd()
try {
    $payload = if ([string]::IsNullOrWhiteSpace($inputText)) { $null } else { $inputText | ConvertFrom-Json }
} catch {
    $payload = $null
}
if ($null -eq $payload -or [string]::IsNullOrWhiteSpace($payload.conversation_id)) { exit 0 }

$sessionId = [string]$payload.conversation_id
if ($sessionId -eq "default") {
    if ([string]::IsNullOrWhiteSpace($payload.agent_id)) { exit 0 }
    $sessionId = "default:" + [string]$payload.agent_id
}

$herdr = if ([string]::IsNullOrWhiteSpace($env:HERDR_BIN_PATH)) { "herdr" } else { $env:HERDR_BIN_PATH }
$source = if ($payload.is_new_session -eq $true) { "new" } else { "resume" }
$commandArgs = @(
    "pane", "report-agent-session", $env:HERDR_PANE_ID,
    "--source", "twodr:letta", "--agent", "letta",
    "--agent-session-id", $sessionId,
    "--seq", [string][DateTime]::UtcNow.Ticks,
    "--session-start-source", $source
)
$job = $null
try {
    $job = Start-Job -ScriptBlock {
        param($Executable, [object[]]$Arguments)
        & $Executable @Arguments *> $null
    } -ArgumentList $herdr, (,$commandArgs)
    if ($null -eq (Wait-Job -Job $job -Timeout 1)) {
        Stop-Job -Job $job
    }
} catch {
    # Hook failures must stay silent because Letta injects output into the next prompt.
    $null = $_
} finally {
    if ($null -ne $job) {
        Remove-Job -Job $job -Force -ErrorAction SilentlyContinue
    }
}
