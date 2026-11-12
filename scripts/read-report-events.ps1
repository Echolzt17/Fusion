param([long]$AcknowledgeCursor = -1, [string]$AcknowledgeBatch = '', [string]$ReportId = '', [switch]$ListReports, [switch]$Reliability, [ValidateRange(0,1000000)][int]$SummaryOffset = 0,
    [long]$ReceivedFrom = 0, [long]$ReceivedUntil = 0, [long]$Before = -1, [string]$BeforeId = '',
    [ValidateRange(0,2000)][int]$ErrorOffset = 0, [ValidateRange(1,200)][int]$ErrorLimit = 50)
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
Add-Type -AssemblyName System.Security
Add-Type -AssemblyName System.Net.Http
. (Join-Path $PSScriptRoot 'report-summary.ps1')
. (Join-Path $PSScriptRoot 'report-monitor-state.ps1')
$reportMonitorRoot = Join-Path $env:LOCALAPPDATA 'FusionReportMonitor'
$reportMonitorCredential = Join-Path $reportMonitorRoot 'admin.dpapi'
if ($ReportId -and $ReportId -notmatch '\A[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}\z') { throw 'Invalid report id.' }
if ($BeforeId -and $BeforeId -notmatch '\A[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}\z') { throw 'Invalid page cursor.' }
if ($AcknowledgeBatch -and $AcknowledgeBatch -notmatch '\A[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}\z') { throw 'Invalid monitor batch id.' }
if (($ListReports -and $ReportId) -or (($ListReports -or $ReportId) -and ($AcknowledgeCursor -ge 0 -or $AcknowledgeBatch))) { throw 'Choose one read operation.' }
if ($Reliability -and ($ListReports -or $ReportId -or $AcknowledgeCursor -ge 0 -or $AcknowledgeBatch)) { throw 'Choose one read operation.' }
if ($ReceivedFrom -lt 0 -or $ReceivedUntil -lt 0 -or (($Before -ge 0) -ne [bool]$BeforeId)) { throw 'Invalid report range or cursor.' }
$reportMonitorReadOnly = $ListReports -or $ReportId -or $Reliability
$reportMonitorLock = $null
$reportMonitorState = $null
$reportMonitorCursor = 0L
if (-not $reportMonitorReadOnly) {
    [IO.Directory]::CreateDirectory($reportMonitorRoot) | Out-Null
    try { $reportMonitorLock = [IO.File]::Open((Join-Path $reportMonitorRoot 'monitor.lock'),[IO.FileMode]::OpenOrCreate,[IO.FileAccess]::ReadWrite,[IO.FileShare]::None) } catch { throw 'Another report check is running; no progress was changed.' }
    try {
        $reportMonitorState = Read-ReportMonitorState $reportMonitorRoot
        $reportMonitorCursor = [long]$reportMonitorState.cursor
        if ($AcknowledgeCursor -ge 0 -or $AcknowledgeBatch) {
            $null = Confirm-ReportMonitorBatch $reportMonitorRoot $reportMonitorState $AcknowledgeBatch $AcknowledgeCursor
            Write-Output 'Report batch acknowledged.'
            $reportMonitorLock.Dispose()
            return
        }
        if ($null -ne $reportMonitorState.pending) {
            $reportMonitorState.pending | ConvertTo-Json -Depth 24 -Compress
            $reportMonitorLock.Dispose()
            return
        }
    } catch { $reportMonitorLock.Dispose(); throw }
}
function Publish-ReportMonitorBatch([string]$Status, [array]$Events, $Health) {
    $batch = New-ReportMonitorBatch $reportMonitorState $Status $Events $Health
    if ($batch.needsAcknowledgement) {
        $reportMonitorState.pending = $batch
        Write-ReportMonitorAtomic (Join-Path $reportMonitorRoot 'monitor-state.json') $reportMonitorState
    }
    $batch | ConvertTo-Json -Depth 24 -Compress
}
$reportMonitorBytes = $null
$reportMonitorHttp = $null
try {
    if (-not (Test-Path -LiteralPath $reportMonitorCredential)) {
        if ($reportMonitorReadOnly) { Write-Output '{"status":"not_configured"}'; return }
        Publish-ReportMonitorBatch 'not_configured' @() $null
        return
    }
    $reportMonitorBytes = [Security.Cryptography.ProtectedData]::Unprotect([IO.File]::ReadAllBytes($reportMonitorCredential), $null, [Security.Cryptography.DataProtectionScope]::CurrentUser)
    $reportMonitorKey = [Text.Encoding]::UTF8.GetString($reportMonitorBytes)
    if ($reportMonitorKey -notmatch '\A[0-9a-f]{64}\z') { throw 'Invalid monitor credential.' }
    $reportMonitorHttp = New-Object Net.Http.HttpClient
    $reportMonitorHttp.Timeout = [TimeSpan]::FromSeconds(20)
    $reportMonitorHttp.MaxResponseContentBufferSize = 1048576
    $reportMonitorHttp.DefaultRequestHeaders.Authorization = New-Object Net.Http.Headers.AuthenticationHeaderValue('Bearer', $reportMonitorKey)
    if ($Reliability) {
        $summaryQuery = "offset=$SummaryOffset&limit=100"
        if ($ReceivedFrom -gt 0) { $summaryQuery += "&from=$ReceivedFrom" }
        if ($ReceivedUntil -gt 0) { $summaryQuery += "&until=$ReceivedUntil" }
        $summaryPage = $reportMonitorHttp.GetStringAsync("https://reports.example.com/v1/admin/reliability?$summaryQuery").GetAwaiter().GetResult() | ConvertFrom-Json
        $summaryPage | ConvertTo-Json -Depth 15 -Compress
        return
    }
    if ($ListReports) {
        $reportListQuery = "from=$ReceivedFrom&limit=50"
        if ($ReceivedUntil -gt 0) { $reportListQuery += "&until=$ReceivedUntil" }
        if ($Before -ge 0) { $reportListQuery += "&before=$Before&beforeId=$BeforeId" }
        $reportList = $reportMonitorHttp.GetStringAsync("https://reports.example.com/v1/admin/reports?$reportListQuery").GetAwaiter().GetResult() | ConvertFrom-Json
        @{status='ok';reports=@($reportList.reports);next=$reportList.next;from=$reportList.from;until=$reportList.until} | ConvertTo-Json -Depth 15 -Compress
        return
    }
    if ($ReportId) {
        $reportMonitorHttp.MaxResponseContentBufferSize = 16777216
        $reportMonitorReportText = $reportMonitorHttp.GetStringAsync("https://reports.example.com/v1/admin/reports/$ReportId").GetAwaiter().GetResult()
        $reportMonitorReport = $reportMonitorReportText | ConvertFrom-Json
        Get-ReportAnalysis -Report $reportMonitorReport -Offset $ErrorOffset -Limit $ErrorLimit | ConvertTo-Json -Depth 15 -Compress
        return
    }
    $reportMonitorText = $reportMonitorHttp.GetStringAsync("https://reports.example.com/v1/admin/events?after=$reportMonitorCursor").GetAwaiter().GetResult()
    $reportMonitorEvents = ($reportMonitorText | ConvertFrom-Json).events
    $reportMonitorHealthText = $reportMonitorHttp.GetStringAsync('https://reports.example.com/v1/admin/health').GetAwaiter().GetResult()
    $reportMonitorHealthValue = $reportMonitorHealthText | ConvertFrom-Json
    Publish-ReportMonitorBatch 'ok' @($reportMonitorEvents) $reportMonitorHealthValue
} catch {
    # Do not expose exception text, headers, token bytes, or raw HTTP bodies.
    if ($Reliability) { Write-Output '{"status":"reliability_unavailable","actionable":true}' }
    elseif ($ListReports) { Write-Output '{"status":"report_list_unavailable","actionable":true}' }
    elseif ($ReportId) { Write-Output '{"status":"report_unavailable","actionable":true}' }
    else { Publish-ReportMonitorBatch 'unreachable_or_unauthorized' @() $null }
} finally {
    if ($null -ne $reportMonitorHttp) { $reportMonitorHttp.Dispose() }
    if ($null -ne $reportMonitorBytes) { [Array]::Clear($reportMonitorBytes,0,$reportMonitorBytes.Length) }
    $reportMonitorKey = $null
    if ($null -ne $reportMonitorLock) { $reportMonitorLock.Dispose() }
}
