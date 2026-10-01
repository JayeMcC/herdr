# installed by herdr
# managed by herdr; reinstalling or updating the integration overwrites this file.
# add custom hooks beside this file instead of editing it.
# TWODR_INTEGRATION_ID=antigravity_cli
# TWODR_INTEGRATION_VERSION=4

# Session-only: this hook reports the Antigravity conversation so Herdr can
# resume the pane. Lifecycle state comes from Herdr's screen detection.

param([string]$Action = "")
# twodr panes export TWODR_* and, until it is dropped, a HERDR_* alias.
# Prefer the twodr name so a pane from either side of that change reports.
if ($env:TWODR_ENV) { $env:HERDR_ENV = $env:TWODR_ENV }
if ($env:TWODR_PANE_ID) { $env:HERDR_PANE_ID = $env:TWODR_PANE_ID }
if ($env:TWODR_BIN_PATH) { $env:HERDR_BIN_PATH = $env:TWODR_BIN_PATH }

# Antigravity CLI expects a JSON object on stdout and this hook never injects
# anything, so every exit path emits an empty object.
function Exit-Hook {
    Write-Output "{}"
    exit 0
}

if ($Action -ne "session") { Exit-Hook }
if ($env:HERDR_ENV -ne "1") { Exit-Hook }
if ([string]::IsNullOrWhiteSpace($env:HERDR_PANE_ID)) { Exit-Hook }

$inputText = [Console]::In.ReadToEnd()
try {
    $payload = if ([string]::IsNullOrWhiteSpace($inputText)) { $null } else { $inputText | ConvertFrom-Json }
} catch {
    Exit-Hook
}

if ($null -eq $payload) { Exit-Hook }

$conversationId = if ($payload.conversationId -is [string]) { $payload.conversationId } else { $null }
if ([string]::IsNullOrWhiteSpace($conversationId)) { Exit-Hook }

$seq = [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds()
$herdr = if ([string]::IsNullOrWhiteSpace($env:HERDR_BIN_PATH)) { "herdr" } else { $env:HERDR_BIN_PATH }
try {
    $sessionArgs = @(
        "pane",
        "report-agent-session",
        $env:HERDR_PANE_ID,
        "--source",
        "twodr:antigravity_cli",
        "--agent",
        "agy",
        "--seq",
        "$seq",
        "--agent-session-id",
        "$conversationId"
    )
    if ($payload.transcriptPath -is [string] -and -not [string]::IsNullOrWhiteSpace($payload.transcriptPath)) {
        $sessionArgs += @("--agent-session-path", "$($payload.transcriptPath)")
    }
    & $herdr @sessionArgs 2>$null | Out-Null
} catch {
}

Exit-Hook
