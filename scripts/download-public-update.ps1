param([Parameter(Mandatory=$true)][string]$Url, [Parameter(Mandatory=$true)][string]$OutFile)
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
# Public release assets only. No GitHub authorization or AI credentials.
if ($Url -notmatch '^https://github\.com/Echolzt17/Fusion/releases/download/v(?<version>\d+\.\d+\.\d+)/(?<asset>[^/?#]+)$') {
    Write-Output 'Public update URL is outside the fixed repository.'
    exit 1
}
$version = $Matches.version
if ($Matches.asset -notin @('manifest.json', "Fusion_${version}_x64-setup.exe", "Fusion_${version}_x64-setup.exe.sig")) {
    Write-Output 'Public update asset is not allowed.'
    exit 1
}
try {
    Invoke-WebRequest -Uri $Url -UseBasicParsing -OutFile $OutFile -TimeoutSec 1800 -MaximumRedirection 5
    if ((Get-Item -LiteralPath $OutFile).Length -gt 512MB) { throw 'Public asset too large' }
} catch {
    Write-Output 'Public update download through the Windows system network failed.'
    exit 1
}
