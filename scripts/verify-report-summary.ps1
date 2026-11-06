$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'report-summary.ps1')
$fixture = [pscustomobject]@{reportId='00000000-0000-4000-8000-000000000001';installationId='00000000-0000-4000-8000-000000000002';preparedAt=1000;environment=@{appVersion='1.0.0'};historyLimited=$false;errors=@(1..25 | ForEach-Object {
    [pscustomobject]@{requestId="fixture-$_";at=900+$_;operation='responses';code=$(if ($_ -eq 25) {'stream_interrupted'} else {'network_error'});failureStage='response_read';networkErrorKind='unexpected_eof';upstreamStatus=200;appVersion=$null;elapsedMs=$_}
})}
$first = Get-ReportAnalysis $fixture -Limit 20
if ($first.errorCount -ne 25 -or $first.errors.Count -ne 20 -or $first.nextOffset -ne 20 -or !$first.summaryComplete) { throw 'Incomplete report accounting.' }
if ($first.groups.Count -ne 2 -or @($first.groups | Where-Object {$_.code -eq 'stream_interrupted'}).Count -ne 1) { throw 'Tail failure missing from summary.' }
if (@($first.groups | Where-Object {$_.failureVersion -ne 'unknown'}).Count -ne 0) { throw 'Historical version was invented.' }
$last = Get-ReportAnalysis $fixture -Offset 20 -Limit 20
if ($last.errors.Count -ne 5 -or $null -ne $last.nextOffset -or $last.deviceAlias -ne $first.deviceAlias) { throw 'Error paging mismatch.' }
if (($first | ConvertTo-Json -Depth 12) -match $fixture.installationId) { throw 'Original installation ID exposed.' }
Write-Output 'Report summary: full classification, pagination, unknown versions and device alias passed.'
