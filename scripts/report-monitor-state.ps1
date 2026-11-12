# Internal monitor state. Reading a batch never acknowledges delivery.
function Write-ReportMonitorAtomic([string]$Path, $Value) {
    $temporary = "$Path.$([guid]::NewGuid().ToString('N')).tmp"
    $bytes = [Text.UTF8Encoding]::new($false).GetBytes(($Value | ConvertTo-Json -Depth 24 -Compress))
    try {
        $stream = [IO.File]::Open($temporary,[IO.FileMode]::CreateNew,[IO.FileAccess]::Write,[IO.FileShare]::None)
        try { $stream.Write($bytes,0,$bytes.Length); $stream.Flush($true) } finally { $stream.Dispose() }
        if ([IO.File]::Exists($Path)) { [IO.File]::Replace($temporary,$Path,[System.Management.Automation.Language.NullString]::Value) } else { [IO.File]::Move($temporary,$Path) }
    } finally { if ([IO.File]::Exists($temporary)) { [IO.File]::Delete($temporary) } }
}
function Read-ReportMonitorState([string]$Root) {
    $path = Join-Path $Root 'monitor-state.json'
    if ([IO.File]::Exists($path)) {
        if ((Get-Item -LiteralPath $path).Length -gt 2097152) { throw 'Monitor state exceeds its limit; original retained.' }
        try { $state = [IO.File]::ReadAllText($path) | ConvertFrom-Json } catch { throw 'Monitor state is invalid; original retained.' }
        if ($state.schemaVersion -ne 1 -or $null -eq $state.cursor -or $state.cursor -lt 0 -or $null -eq $state.health) { throw 'Monitor state version or fields are invalid; original retained.' }
        if ($null -ne $state.pending -and ($state.pending.batchId -notmatch '\A[0-9a-f-]{36}\z' -or $state.pending.afterCursor -ne $state.cursor -or $state.pending.nextCursor -lt $state.cursor)) { throw 'Monitor pending batch is invalid; original retained.' }
        return $state
    }
    $cursor = 0L; $health = ''
    $legacyCursor = Join-Path $Root 'cursor.json'
    if ([IO.File]::Exists($legacyCursor)) {
        try { $value = [IO.File]::ReadAllText($legacyCursor) | ConvertFrom-Json; if ($null -eq $value.cursor -or $value.cursor -lt 0) { throw 'invalid' }; $cursor = [long]$value.cursor } catch { throw 'Legacy monitor cursor is invalid; original retained.' }
    }
    $legacyHealth = Join-Path $Root 'health-state'
    if ([IO.File]::Exists($legacyHealth)) { $health = [IO.File]::ReadAllText($legacyHealth) }
    return [pscustomobject]@{schemaVersion=1;cursor=$cursor;health=$health;pending=$null;lastConfirmedBatch=$null}
}
function New-ReportMonitorBatch($State, [string]$Status, [array]$Events, $Health) {
    $maximum = [long]$State.cursor
    $previous = [long]$State.cursor
    foreach ($event in $Events) {
        if ($null -eq $event.id -or [long]$event.id -le $previous) { throw 'Monitor event order is invalid; nothing acknowledged.' }
        $previous = [long]$event.id; $maximum = $previous
    }
    $healthValue = if ($Status -eq 'ok') { 'ok:' + (@($Health.issues | Sort-Object) -join ',') } else { $Status }
    $changed = $State.health -ne $healthValue
    $cutover = if ($null -ne $Health.immediateErrorsAfterCursor) { [long]$Health.immediateErrorsAfterCursor } else { [long]::MaxValue }
    $actionableEvents = @($Events | Where-Object { $_.kind -eq 'new_errors' -or (($_.kind -eq 'early' -or $_.kind -eq 'daily') -and [long]$_.id -le $cutover) })
    $actionable = $changed -or $actionableEvents.Count -gt 0
    # Suppressed daily/early events still need a confirmed cursor advance.
    if (-not $actionable -and $Events.Count -eq 0) {
        return [pscustomobject]@{status=$Status;events=@();health=$Health;afterCursor=$State.cursor;healthChanged=$false;actionable=$false;batchId=$null;nextCursor=$maximum;needsAcknowledgement=$false}
    }
    return [pscustomobject]@{status=$Status;events=$actionableEvents;health=$Health;afterCursor=$State.cursor;healthChanged=$changed;actionable=$actionable;batchId=[guid]::NewGuid().ToString();nextCursor=$maximum;nextHealth=$healthValue;needsAcknowledgement=$true}
}
function Confirm-ReportMonitorBatch([string]$Root, $State, [string]$BatchId, [long]$LegacyCursor = -1) {
    if ($null -eq $State.pending) {
        if ($BatchId -and $State.lastConfirmedBatch -eq $BatchId) { return $State }
        throw 'No matching pending monitor batch; nothing acknowledged.'
    }
    if (($BatchId -and $BatchId -ne $State.pending.batchId) -or (-not $BatchId -and $LegacyCursor -ne $State.pending.nextCursor)) { throw 'Monitor batch does not match; nothing acknowledged.' }
    $State.cursor = [long]$State.pending.nextCursor
    $State.health = $State.pending.nextHealth
    $State.lastConfirmedBatch = $State.pending.batchId
    $State.pending = $null
    Write-ReportMonitorAtomic (Join-Path $Root 'monitor-state.json') $State
    return $State
}
