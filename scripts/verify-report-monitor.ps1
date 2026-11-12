$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'report-monitor-state.ps1')
$fixtureRoot = Join-Path ([IO.Path]::GetTempPath()) ('fusion-monitor-test-' + [guid]::NewGuid().ToString('N'))
[IO.Directory]::CreateDirectory($fixtureRoot) | Out-Null
function Assert-Monitor([bool]$Condition, [string]$Message) { if (-not $Condition) { throw $Message } }
function Assert-MonitorFailure([scriptblock]$Action) { $failed=$false; try { & $Action | Out-Null } catch { $failed=$true }; Assert-Monitor $failed 'Expected protected monitor operation to fail.' }
try {
    [IO.File]::WriteAllText((Join-Path $fixtureRoot 'cursor.json'),'{"cursor":9}')
    [IO.File]::WriteAllText((Join-Path $fixtureRoot 'health-state'),'ok:')
    $state = Read-ReportMonitorState $fixtureRoot
    Assert-Monitor ($state.cursor -eq 9 -and $state.health -eq 'ok:') 'Legacy progress was not preserved.'
    Assert-Monitor (-not (Test-Path -LiteralPath (Join-Path $fixtureRoot 'monitor-state.json'))) 'Read acknowledged progress.'
    $health = [pscustomobject]@{issues=@();immediateErrorsAfterCursor=9}
    $batch = New-ReportMonitorBatch $state 'ok' @([pscustomobject]@{id=10;kind='new_errors';summary=@{acceptedErrors=1}},[pscustomobject]@{id=11;kind='daily'}) $health
    Assert-Monitor ($batch.actionable -and $batch.events.Count -eq 1 -and $batch.nextCursor -eq 11) 'Immediate and daily events were repeated or lost.'
    $state.pending=$batch
    Write-ReportMonitorAtomic (Join-Path $fixtureRoot 'monitor-state.json') $state
    $restarted = Read-ReportMonitorState $fixtureRoot
    Assert-Monitor ($restarted.cursor -eq 9 -and $restarted.pending.batchId -eq $batch.batchId) 'Failed delivery advanced or replaced the pending batch.'
    Assert-MonitorFailure { Confirm-ReportMonitorBatch $fixtureRoot $restarted ([guid]::NewGuid().ToString()) }
    Assert-Monitor ((Read-ReportMonitorState $fixtureRoot).cursor -eq 9) 'Wrong batch advanced progress.'
    $state = Confirm-ReportMonitorBatch $fixtureRoot $restarted $batch.batchId
    Assert-Monitor ($state.cursor -eq 11 -and $null -eq $state.pending) 'Confirmed batch did not commit.'
    $state = Confirm-ReportMonitorBatch $fixtureRoot $state $batch.batchId
    $silent = New-ReportMonitorBatch $state 'ok' @() $health
    Assert-Monitor (-not $silent.actionable -and -not $silent.needsAcknowledgement) 'Unchanged health generated a routine notification.'
    $daily = New-ReportMonitorBatch $state 'ok' @([pscustomobject]@{id=12;kind='daily'}) $health
    Assert-Monitor (-not $daily.actionable -and $daily.needsAcknowledgement -and $daily.nextCursor -eq 12) 'Suppressed summaries cannot safely advance.'
    $failure = New-ReportMonitorBatch $state 'unreachable_or_unauthorized' @() $null
    Assert-Monitor ($failure.actionable -and $state.health -eq 'ok:' -and $state.cursor -eq 11) 'Read consumed health failure before delivery.'
    $state.pending=$failure
    Write-ReportMonitorAtomic (Join-Path $fixtureRoot 'monitor-state.json') $state
    $state = Confirm-ReportMonitorBatch $fixtureRoot (Read-ReportMonitorState $fixtureRoot) $failure.batchId
    Assert-Monitor (-not (New-ReportMonitorBatch $state 'unreachable_or_unauthorized' @() $null).actionable) 'Acknowledged outage repeated.'
    Assert-Monitor (New-ReportMonitorBatch $state 'ok' @() $health).actionable 'Recovery was not actionable.'
    Assert-MonitorFailure { New-ReportMonitorBatch $state 'ok' @([pscustomobject]@{id=11;kind='new_errors'}) $health }
    $future = '{"schemaVersion":2,"cursor":11,"health":"ok:","pending":null}'
    [IO.File]::WriteAllText((Join-Path $fixtureRoot 'monitor-state.json'),$future)
    Assert-MonitorFailure { Read-ReportMonitorState $fixtureRoot }
    Assert-Monitor ([IO.File]::ReadAllText((Join-Path $fixtureRoot 'monitor-state.json')) -eq $future) 'Future state was overwritten.'
    Write-Output 'Report monitor: legacy migration, staged health, durable batches, matching acknowledgement, deduplication and version protection passed.'
} finally {
    $resolvedFixture = [IO.Path]::GetFullPath($fixtureRoot)
    $resolvedTemp = [IO.Path]::GetFullPath([IO.Path]::GetTempPath()).TrimEnd('\') + '\'
    if ($resolvedFixture.StartsWith($resolvedTemp,[StringComparison]::OrdinalIgnoreCase) -and [IO.Path]::GetFileName($resolvedFixture).StartsWith('fusion-monitor-test-')) { Remove-Item -LiteralPath $resolvedFixture -Recurse -Force }
}
