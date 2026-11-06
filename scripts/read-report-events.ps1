param([long]$AcknowledgeCursor = -1, [string]$ReportId = '', [switch]$ListReports, [switch]$Reliability, [ValidateRange(0,1000000)][int]$SummaryOffset = 0,
    [long]$ReceivedFrom = 0, [long]$ReceivedUntil = 0, [long]$Before = -1, [string]$BeforeId = '',
    [ValidateRange(0,2000)][int]$ErrorOffset = 0, [ValidateRange(1,200)][int]$ErrorLimit = 50)
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
Add-Type -AssemblyName System.Security
Add-Type -AssemblyName System.Net.Http
. (Join-Path $PSScriptRoot 'report-summary.ps1')
$reportMonitorRoot = Join-Path $env:LOCALAPPDATA 'FusionReportMonitor'
$reportMonitorState = Join-Path $reportMonitorRoot 'cursor.json'
$reportMonitorCredential = Join-Path $reportMonitorRoot 'admin.dpapi'
if ($ReportId -and $ReportId -notmatch '\A[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}\z') { throw 'Invalid report id.' }
if ($BeforeId -and $BeforeId -notmatch '\A[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}\z') { throw 'Invalid page cursor.' }
if (($ListReports -and $ReportId) -or (($ListReports -or $ReportId) -and $AcknowledgeCursor -ge 0)) { throw 'Choose one read operation.' }
if ($Reliability -and ($ListReports -or $ReportId -or $AcknowledgeCursor -ge 0)) { throw 'Choose one read operation.' }
if ($ReceivedFrom -lt 0 -or $ReceivedUntil -lt 0 -or (($Before -ge 0) -ne [bool]$BeforeId)) { throw 'Invalid report range or cursor.' }
$reportMonitorHealth = Join-Path $reportMonitorRoot 'health-state'
function Set-ReportMonitorHealth([string]$Value) {
    $old = if (Test-Path -LiteralPath $reportMonitorHealth) { [IO.File]::ReadAllText($reportMonitorHealth) } else { '' }
    if ($old -eq $Value) { return $false }
    [IO.Directory]::CreateDirectory($reportMonitorRoot) | Out-Null
    [IO.File]::WriteAllText($reportMonitorHealth,$Value)
    return $true
}
$reportMonitorCursor = 0L
if (Test-Path -LiteralPath $reportMonitorState) {
    try { $reportMonitorCursor = [long](([IO.File]::ReadAllText($reportMonitorState) | ConvertFrom-Json).cursor) } catch { throw 'Report monitor cursor is invalid; it was not replaced.' }
}
if ($AcknowledgeCursor -ge 0) {
    if ($AcknowledgeCursor -lt $reportMonitorCursor) { throw 'Report monitor cursor cannot move backwards.' }
    $reportMonitorTemp = "$reportMonitorState.tmp"
    [IO.File]::WriteAllText($reportMonitorTemp, (@{cursor=$AcknowledgeCursor} | ConvertTo-Json -Compress))
    Move-Item -LiteralPath $reportMonitorTemp -Destination $reportMonitorState -Force
    Write-Output 'Report cursor acknowledged.'
    exit 0
}
if (-not (Test-Path -LiteralPath $reportMonitorCredential)) {
    if ($ListReports -or $ReportId -or $Reliability) { Write-Output '{"status":"not_configured"}'; exit 0 }
    @{status='not_configured';events=@();actionable=(Set-ReportMonitorHealth 'not_configured')} | ConvertTo-Json -Compress
    exit 0
}
$reportMonitorBytes = $null
$reportMonitorHttp = $null
try {
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
    $reportMonitorRecovered = Set-ReportMonitorHealth ('ok:' + (@($reportMonitorHealthValue.issues | Sort-Object) -join ','))
    @{status='ok';events=@($reportMonitorEvents);health=$reportMonitorHealthValue;afterCursor=$reportMonitorCursor;healthChanged=$reportMonitorRecovered;actionable=(@($reportMonitorEvents).Count -gt 0 -or $reportMonitorRecovered)} | ConvertTo-Json -Depth 15 -Compress
} catch {
    # Do not expose exception text, headers, token bytes, or raw HTTP bodies.
    if ($Reliability) { Write-Output '{"status":"reliability_unavailable","actionable":true}' }
    elseif ($ListReports) { Write-Output '{"status":"report_list_unavailable","actionable":true}' }
    elseif ($ReportId) { Write-Output '{"status":"report_unavailable","actionable":true}' }
    else { @{status='unreachable_or_unauthorized';events=@();actionable=(Set-ReportMonitorHealth 'unreachable_or_unauthorized')} | ConvertTo-Json -Compress }
} finally {
    if ($null -ne $reportMonitorHttp) { $reportMonitorHttp.Dispose() }
    if ($null -ne $reportMonitorBytes) { [Array]::Clear($reportMonitorBytes,0,$reportMonitorBytes.Length) }
    $reportMonitorKey = $null
}
