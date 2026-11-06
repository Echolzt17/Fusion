$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'backup-update-signing.ps1') -LibraryOnly
$temporary = Join-Path ([IO.Path]::GetTempPath()) ('fusion-signing-fixture-' + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $temporary | Out-Null
try {
    $source = Join-Path $temporary 'source'; New-Item -ItemType Directory -Path $source | Out-Null
    [IO.File]::WriteAllText((Join-Path $source 'fusion.key'),'public fake encrypted key')
    [IO.File]::WriteAllText((Join-Path $source 'fusion.key.pub'),'public fixture key')
    [IO.File]::WriteAllBytes((Join-Path $source 'password.dpapi'),[Security.Cryptography.ProtectedData]::Protect([Text.Encoding]::UTF8.GetBytes('public fixture original password'),$null,[Security.Cryptography.DataProtectionScope]::CurrentUser))
    $password = [Security.SecureString]::new()
    'public fixture backup password'.ToCharArray() | ForEach-Object { $password.AppendChar($_) }
    $password.MakeReadOnly()
    $backup = Join-Path $temporary 'offline-backup.json'
    Export-FusionSigningBackup $source $backup $password
    if ([IO.File]::ReadAllText($backup).Contains('public fixture original password')) { throw 'Backup exposed its payload.' }
    $restored = Join-Path $temporary 'restored'
    Restore-FusionSigningBackup $backup $restored $password 'public fixture key'
    if ([IO.File]::ReadAllText((Join-Path $restored 'fusion.key')) -ne 'public fake encrypted key') { throw 'Restored key mismatch.' }
    $decoded = [Text.Encoding]::UTF8.GetString([Security.Cryptography.ProtectedData]::Unprotect([IO.File]::ReadAllBytes((Join-Path $restored 'password.dpapi')),$null,[Security.Cryptography.DataProtectionScope]::CurrentUser))
    if ($decoded -ne 'public fixture original password') { throw 'Restored password mismatch.' }
    $failed = $false
    $wrongPassword=[Security.SecureString]::new()
    'wrong fixture password'.ToCharArray() | ForEach-Object { $wrongPassword.AppendChar($_) }
    try { Restore-FusionSigningBackup $backup (Join-Path $temporary 'wrong') $wrongPassword 'public fixture key' } catch { $failed=$true }
    if (-not $failed -or (Test-Path -LiteralPath (Join-Path $temporary 'wrong'))) { throw 'Wrong password was not safely rejected.' }
    $failed=$false
    try { Export-FusionSigningBackup $source $backup $password } catch { $failed=$true }
    if (-not $failed) { throw 'Existing backup was overwritten.' }
    Write-Output 'Signing backup: isolated CMS/PFX round-trip, wrong password and overwrite guard passed. No real signing files were read.'
} finally {
    $resolved = [IO.Path]::GetFullPath($temporary)
    $expectedParent = [IO.Path]::GetFullPath([IO.Path]::GetTempPath()).TrimEnd('\')
    if ((Split-Path -Parent $resolved) -eq $expectedParent -and (Split-Path -Leaf $resolved).StartsWith('fusion-signing-fixture-')) { Remove-Item -LiteralPath $resolved -Recurse -Force }
}
