$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'publish-credential.ps1')
$testRoot = Join-Path ([IO.Path]::GetTempPath()) ('fusion-publish-credential-' + [Guid]::NewGuid().ToString('N'))
$path = Join-Path $testRoot 'public-fixture.dpapi'
try {
    if ($null -ne (Read-FusionPublishToken $path)) { throw 'Missing fixture should be unknown' }
    $first = 'github_pat_PUBLIC_TEST_FIXTURE_ONE'
    $second = 'github_pat_PUBLIC_TEST_FIXTURE_TWO'
    Save-FusionPublishToken $path $first
    if ((Read-FusionPublishToken $path) -cne $first) { throw 'DPAPI round trip failed' }
    $bytes = [IO.File]::ReadAllBytes($path)
    if ([Text.Encoding]::UTF8.GetString($bytes).Contains($first) -or [Text.Encoding]::Unicode.GetString($bytes).Contains($first)) { throw 'Plaintext leaked' }
    Save-FusionPublishToken $path $second
    if ((Read-FusionPublishToken $path) -cne $second) { throw 'Atomic replacement failed' }
    Save-FusionPublishToken $path 'gho_PUBLIC_TEST_FIXTURE'
    if ((Read-FusionPublishToken $path) -cne 'gho_PUBLIC_TEST_FIXTURE') { throw 'Existing OAuth authorization could not be protected' }
    Save-FusionPublishToken $path $second
    $rejected = $false
    try { Save-FusionPublishToken $path 'invalid-public-fixture' } catch { $rejected = $true }
    if (-not $rejected -or (Read-FusionPublishToken $path) -cne $second) { throw 'Invalid replacement changed the saved fixture' }
    [IO.File]::WriteAllBytes($path, [byte[]](1,2,3,4))
    $rejected = $false
    try { Read-FusionPublishToken $path | Out-Null } catch {
        $rejected = $true
        if ($_.Exception.Message -ne 'Saved publisher credential is unavailable. Enter a replacement in the local masked window.') { throw 'Unexpected diagnostic detail' }
    }
    if (-not $rejected -or [IO.File]::ReadAllBytes($path).Length -ne 4) { throw 'Corrupt fixture was accepted or changed' }
    if (@(Get-ChildItem -LiteralPath $testRoot).Count -ne 1) { throw 'Temporary credential file leaked' }
    Write-Output 'Publisher credential: temporary public-fixture DPAPI round trip, ciphertext, replacement and corruption checks passed. No real credentials used.'
} finally {
    if ([IO.File]::Exists($path)) { [IO.File]::Delete($path) }
    if ([IO.Directory]::Exists($testRoot)) { [IO.Directory]::Delete($testRoot) }
}
