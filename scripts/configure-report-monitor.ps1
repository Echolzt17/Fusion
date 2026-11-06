param([string]$TemporarySshDirectory = (Join-Path $env:LOCALAPPDATA 'FusionDeployment\example'))
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Security
$reportMonitorRoot = Join-Path $env:LOCALAPPDATA 'FusionReportMonitor'
$reportMonitorCredential = Join-Path $reportMonitorRoot 'admin.dpapi'
if (Test-Path -LiteralPath $reportMonitorCredential) { throw 'Monitor already configured; existing credential was preserved.' }
$reportDeployKey = Join-Path $TemporarySshDirectory 'fusion-report-deploy'
$reportKnownHosts = Join-Path $TemporarySshDirectory 'known_hosts'
if (-not (Test-Path -LiteralPath $reportDeployKey)) { throw 'Temporary deployment key is unavailable.' }
$reportSecretBytes = New-Object byte[] 32
$reportRng = [Security.Cryptography.RandomNumberGenerator]::Create()
$reportRng.GetBytes($reportSecretBytes)
$reportSecret = ([BitConverter]::ToString($reportSecretBytes)).Replace('-','').ToLowerInvariant()
$reportPlainBytes = [Text.Encoding]::UTF8.GetBytes($reportSecret)
$reportCipher = [Security.Cryptography.ProtectedData]::Protect($reportPlainBytes,$null,[Security.Cryptography.DataProtectionScope]::CurrentUser)
[IO.Directory]::CreateDirectory($reportMonitorRoot) | Out-Null
$reportPending = Join-Path $reportMonitorRoot 'admin.pending.dpapi'
[IO.File]::WriteAllBytes($reportPending,$reportCipher)
try {
    $reportRemoteCommand = 'set -eu; if ! id fusion-reports >/dev/null 2>&1; then sudo useradd --system --home /var/lib/fusion-reports --shell /usr/sbin/nologin fusion-reports; fi; sudo install -d -m 0750 -o root -g fusion-reports /etc/fusion-reports; sudo test ! -e /etc/fusion-reports/admin.key; sudo sh -c ''umask 077; cat > /etc/fusion-reports/admin.key''; sudo chown root:fusion-reports /etc/fusion-reports/admin.key; sudo chmod 0640 /etc/fusion-reports/admin.key; printf MONITOR_KEY_INSTALLED'
    $reportProcessInfo = New-Object Diagnostics.ProcessStartInfo
    $reportProcessInfo.FileName = 'ssh.exe'
    $reportProcessInfo.Arguments = '-i "' + $reportDeployKey + '" -o KexAlgorithms=curve25519-sha256 -o StrictHostKeyChecking=yes -o UserKnownHostsFile="' + $reportKnownHosts + '" -o IdentitiesOnly=yes -o BatchMode=yes -o ConnectTimeout=10 ubuntu@reports.example.com "' + $reportRemoteCommand + '"'
    $reportProcessInfo.UseShellExecute = $false
    $reportProcessInfo.CreateNoWindow = $true
    $reportProcessInfo.RedirectStandardInput = $true
    $reportProcessInfo.RedirectStandardOutput = $true
    $reportProcessInfo.RedirectStandardError = $true
    $reportProcess = [Diagnostics.Process]::Start($reportProcessInfo)
    $reportProcess.StandardInput.WriteLine($reportSecret)
    $reportProcess.StandardInput.Close()
    $reportOutputTask = $reportProcess.StandardOutput.ReadToEndAsync()
    $reportErrorTask = $reportProcess.StandardError.ReadToEndAsync()
    if (-not $reportProcess.WaitForExit(30000)) { $reportProcess.Kill(); throw 'Monitor setup timed out; encrypted pending material was preserved.' }
    $reportOutput = $reportOutputTask.GetAwaiter().GetResult()
    $null = $reportErrorTask.GetAwaiter().GetResult()
    if ($reportProcess.ExitCode -ne 0 -or $reportOutput -notmatch 'MONITOR_KEY_INSTALLED') { throw 'Monitor setup failed; encrypted pending material was preserved.' }
    Move-Item -LiteralPath $reportPending -Destination $reportMonitorCredential
    Write-Output 'Monitor read credential installed and stored with CurrentUser DPAPI; no credential was displayed.'
} finally {
    [Array]::Clear($reportSecretBytes,0,$reportSecretBytes.Length)
    [Array]::Clear($reportPlainBytes,0,$reportPlainBytes.Length)
    $reportSecret = $null
    $reportRng.Dispose()
}
