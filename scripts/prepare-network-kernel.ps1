param([switch]$CheckOnly)
$ErrorActionPreference = 'Stop'
function Get-PinnedFileSha256([string]$Path) {
    $stream = [IO.File]::OpenRead($Path)
    $algorithm = [Security.Cryptography.SHA256]::Create()
    try { return [BitConverter]::ToString($algorithm.ComputeHash($stream)).Replace('-', '').ToLowerInvariant() }
    finally { $algorithm.Dispose(); $stream.Dispose() }
}
$version = 'v1.19.33'
$archiveName = "mihomo-windows-amd64-compatible-$version.zip"
$archiveSha256 = '7aaf636a4f2172dcd8ec34ac4f52797b059195e5afd6409508c963e545f8864d'
$executableSha256 = '926bfea2ce48a247d6a7ce4cf641980e416202d5e40fe4ee5c949089ace0e197'
$resourceRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..\resources\network-kernel'))
$binary = Join-Path $resourceRoot 'mihomo.exe'
$manifest = Join-Path $resourceRoot 'manifest.json'
if (Test-Path -LiteralPath $manifest) {
    $saved = Get-Content -LiteralPath $manifest -Raw | ConvertFrom-Json
    if ($saved.version -ne $version -or $saved.archiveSha256 -ne $archiveSha256 -or $saved.executableSha256 -ne $executableSha256) { throw 'Bundled network kernel metadata does not match the pinned release.' }
    if ((Test-Path -LiteralPath $binary) -and (Test-Path -LiteralPath (Join-Path $resourceRoot 'LICENSE')) -and ((Get-PinnedFileSha256 $binary) -eq $executableSha256)) {
        Write-Host "Bundled network kernel $version verified."
        exit 0
    }
}
if ($CheckOnly) { throw 'Pinned network kernel is missing or invalid. Run scripts/prepare-network-kernel.ps1 to prepare it.' }
[IO.Directory]::CreateDirectory($resourceRoot) | Out-Null
$temporaryRoot = Join-Path ([IO.Path]::GetTempPath()) ('fusion-kernel-download-' + [Guid]::NewGuid().ToString('N'))
[IO.Directory]::CreateDirectory($temporaryRoot) | Out-Null
try {
    $archive = Join-Path $temporaryRoot $archiveName
    $download = "https://github.com/MetaCubeX/mihomo/releases/download/$version/$archiveName"
    Invoke-WebRequest -Uri $download -OutFile $archive -UseBasicParsing -TimeoutSec 180
    if ((Get-PinnedFileSha256 $archive) -ne $archiveSha256) { throw 'Pinned network kernel archive digest mismatch.' }
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $zip = [IO.Compression.ZipFile]::OpenRead($archive)
    try {
        $entries = @($zip.Entries | Where-Object { $_.FullName -match '^mihomo-windows-amd64-compatible(?:-v1\.19\.33)?\.exe$' })
        if ($entries.Count -ne 1) { throw 'Pinned network kernel archive structure mismatch.' }
        [IO.Compression.ZipFileExtensions]::ExtractToFile($entries[0], $binary, $true)
    } finally { $zip.Dispose() }
    Invoke-WebRequest -Uri "https://raw.githubusercontent.com/MetaCubeX/mihomo/$version/LICENSE" -OutFile (Join-Path $resourceRoot 'LICENSE') -UseBasicParsing -TimeoutSec 30
    $binaryHash = Get-PinnedFileSha256 $binary
    if ($binaryHash -ne $executableSha256) { throw 'Pinned network kernel executable digest mismatch.' }
    $metadata = [ordered]@{
        version = $version
        platform = 'windows-amd64-compatible'
        executable = 'mihomo.exe'
        archiveUrl = $download
        archiveSha256 = $archiveSha256
        executableSha256 = $binaryHash
        license = 'GPL-3.0'
        licenseUrl = "https://github.com/MetaCubeX/mihomo/blob/$version/LICENSE"
        sourceUrl = "https://github.com/MetaCubeX/mihomo/archive/refs/tags/$version.zip"
    } | ConvertTo-Json
    [IO.File]::WriteAllText($manifest, $metadata, (New-Object Text.UTF8Encoding($false)))
    Write-Host "Prepared pinned network kernel $version ($binaryHash)."
} finally {
    # Only this explicitly created temporary directory is removed.
    $resolvedTemporary = [IO.Path]::GetFullPath($temporaryRoot)
    $expectedParent = [IO.Path]::GetFullPath([IO.Path]::GetTempPath()).TrimEnd('\') + '\'
    if (-not $resolvedTemporary.StartsWith($expectedParent, [StringComparison]::OrdinalIgnoreCase) -or [IO.Path]::GetFileName($resolvedTemporary) -notlike 'fusion-kernel-download-*') { throw 'Temporary directory verification failed.' }
    Remove-Item -LiteralPath $resolvedTemporary -Recurse -Force
}
