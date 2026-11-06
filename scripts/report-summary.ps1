# Pure projections for private report inspection. Never infer historical versions from environment.
function Get-ReportAnalysis($Report, [int]$Offset = 0, [int]$Limit = 50) {
    if ($Offset -lt 0 -or $Offset -gt 2000 -or $Limit -lt 1 -or $Limit -gt 200) { throw 'Invalid error page.' }
    $errors = @($Report.errors)
    if ($errors.Count -gt 2000) { throw 'Report exceeds the record limit.' }
    $sha = [Security.Cryptography.SHA256]::Create()
    try { $device = 'device-' + ([BitConverter]::ToString($sha.ComputeHash([Text.Encoding]::UTF8.GetBytes($Report.installationId)))).Replace('-','').ToLowerInvariant() }
    finally { $sha.Dispose() }
    $groups = @($errors | Group-Object -Property operation,code,failureStage,networkErrorKind,upstreamStatus,appVersion,{ $_.details.terminalEvent },{ $_.details.upstreamFailure } | ForEach-Object {
        $first = $_.Group[0]
        $duration = $_.Group | Measure-Object -Property elapsedMs -Minimum -Maximum -Average
        $times = $_.Group | Measure-Object -Property at -Minimum -Maximum
        @{operation=$first.operation;code=$first.code;failureStage=$first.failureStage;networkErrorKind=$first.networkErrorKind;
            upstreamStatus=$first.upstreamStatus;failureVersion=$(if ($first.appVersion) {$first.appVersion} else {'unknown'});
            terminalEvent=$first.details.terminalEvent;upstreamFailure=$first.details.upstreamFailure;
            count=$_.Count;firstAt=$times.Minimum;lastAt=$times.Maximum;
            elapsedMs=@{min=$duration.Minimum;max=$duration.Maximum;mean=[math]::Round($duration.Average,1)}}
    })
    $page = @($errors | Select-Object -Skip $Offset -First $Limit)
    $next = if ($Offset + $page.Count -lt $errors.Count) { $Offset + $page.Count } else { $null }
    @{status='ok';reportId=$Report.reportId;deviceAlias=$device;preparedAt=$Report.preparedAt;
        environment=$Report.environment;errorCount=$errors.Count;historyLimited=$Report.historyLimited;
        reliability=$Report.reliability;summaryComplete=$true;groups=$groups;errors=$page;offset=$Offset;nextOffset=$next;
        truncated=($Offset -gt 0 -or $null -ne $next)}
}
