# installed by herdr
# managed by herdr; reinstalling or updating the integration overwrites this file.
# add custom hooks beside this file instead of editing it.
# TWODR_INTEGRATION_ID=qodercli
# TWODR_INTEGRATION_VERSION=4

param([string]$Action = "")
# twodr panes export TWODR_* and, until it is dropped, a HERDR_* alias.
# Prefer the twodr name so a pane from either side of that change reports.
if ($env:TWODR_ENV) { $env:HERDR_ENV = $env:TWODR_ENV }
if ($env:TWODR_PANE_ID) { $env:HERDR_PANE_ID = $env:TWODR_PANE_ID }
if ($env:TWODR_BIN_PATH) { $env:HERDR_BIN_PATH = $env:TWODR_BIN_PATH }

if ($Action -ne "session") { exit 0 }
if ($env:HERDR_ENV -ne "1") { exit 0 }
if ([string]::IsNullOrWhiteSpace($env:HERDR_PANE_ID)) { exit 0 }

$inputText = [Console]::In.ReadToEnd()
try {
    $payload = if ([string]::IsNullOrWhiteSpace($inputText)) { $null } else { $inputText | ConvertFrom-Json }
} catch {
    $payload = $null
}

if ($null -eq $payload -or [string]::IsNullOrWhiteSpace($payload.session_id)) { exit 0 }

$seq = [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds()
$herdr = if ([string]::IsNullOrWhiteSpace($env:HERDR_BIN_PATH)) { "herdr" } else { $env:HERDR_BIN_PATH }
try {
    & $herdr pane report-agent-session $env:HERDR_PANE_ID --source twodr:qodercli --agent qodercli --agent-session-id $payload.session_id --seq $seq 2>$null | Out-Null
} catch {
}
