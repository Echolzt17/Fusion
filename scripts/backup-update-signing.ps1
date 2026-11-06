param([string]$BackupPath, [switch]$Restore, [switch]$LibraryOnly)
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Security

# Portable backup uses Windows CMS and a password-protected PKCS#12 key,
# rather than copying a DPAPI blob that only the original user can decrypt.
function Export-FusionSigningBackup([string]$Directory, [string]$Destination, [Security.SecureString]$Password) {
    if (Test-Path -LiteralPath $Destination) { throw 'Backup destination already exists; not overwritten.' }
    $plainPassword = (New-Object Management.Automation.PSCredential('backup',$Password)).GetNetworkCredential().Password
    if ($plainPassword.Length -lt 12) { throw 'Use a backup password of at least 12 characters and keep it in your password manager.' }
    $originalPassword = [Text.Encoding]::UTF8.GetString([Security.Cryptography.ProtectedData]::Unprotect([IO.File]::ReadAllBytes((Join-Path $Directory 'password.dpapi')), $null, [Security.Cryptography.DataProtectionScope]::CurrentUser))
    $payload = @{ key=[IO.File]::ReadAllText((Join-Path $Directory 'fusion.key')); public=[IO.File]::ReadAllText((Join-Path $Directory 'fusion.key.pub')).Trim(); password=$originalPassword } | ConvertTo-Json -Compress
    $rsa = [Security.Cryptography.RSACng]::new(3072)
    $certificate = $null
    try {
        $request = [Security.Cryptography.X509Certificates.CertificateRequest]::new('CN=Fusion signing recovery', $rsa, [Security.Cryptography.HashAlgorithmName]::SHA256, [Security.Cryptography.RSASignaturePadding]::Pkcs1)
        $certificate = $request.CreateSelfSigned([DateTimeOffset]::UtcNow.AddMinutes(-5), [DateTimeOffset]::UtcNow.AddYears(20))
        $content = [Security.Cryptography.Pkcs.ContentInfo]::new([Text.Encoding]::UTF8.GetBytes($payload))
        $algorithm = [Security.Cryptography.Pkcs.AlgorithmIdentifier]::new([Security.Cryptography.Oid]::new('2.16.840.1.101.3.4.1.42'))
        $envelope = [Security.Cryptography.Pkcs.EnvelopedCms]::new($content, $algorithm)
        $envelope.Encrypt([Security.Cryptography.Pkcs.CmsRecipient]::new($certificate))
        $backup = @{ format='Fusion-signing-CMS-PFX-v1'; envelope=[Convert]::ToBase64String($envelope.Encode()); certificate=[Convert]::ToBase64String($certificate.Export([Security.Cryptography.X509Certificates.X509ContentType]::Pfx,$plainPassword)) } | ConvertTo-Json -Compress
        $output = [IO.File]::Open($Destination,[IO.FileMode]::CreateNew,[IO.FileAccess]::Write,[IO.FileShare]::None)
        try { $bytes=[Text.Encoding]::UTF8.GetBytes($backup); $output.Write($bytes,0,$bytes.Length); $output.Flush($true) } finally { $output.Dispose() }
    } finally { if ($certificate) { $certificate.Dispose() }; $rsa.Dispose(); $plainPassword=$null; $originalPassword=$null; $payload=$null }
}
function Restore-FusionSigningBackup([string]$Source, [string]$Directory, [Security.SecureString]$Password, [string]$ExpectedPublicKey) {
    if (Test-Path -LiteralPath (Join-Path $Directory 'fusion.key')) { throw 'Signing key already exists; restore will not overwrite it.' }
    $backup = [IO.File]::ReadAllText($Source) | ConvertFrom-Json
    if ($backup.format -ne 'Fusion-signing-CMS-PFX-v1') { throw 'Unknown signing backup format.' }
    $plainPassword = (New-Object Management.Automation.PSCredential('backup',$Password)).GetNetworkCredential().Password
    $certificate = $null
    try {
        $certificate = [Security.Cryptography.X509Certificates.X509Certificate2]::new([Convert]::FromBase64String($backup.certificate),$plainPassword,[Security.Cryptography.X509Certificates.X509KeyStorageFlags]::EphemeralKeySet)
        $envelope = [Security.Cryptography.Pkcs.EnvelopedCms]::new()
        $envelope.Decode([Convert]::FromBase64String($backup.envelope))
        $envelope.Decrypt([Security.Cryptography.X509Certificates.X509Certificate2Collection]::new($certificate))
        $payload = [Text.Encoding]::UTF8.GetString($envelope.ContentInfo.Content) | ConvertFrom-Json
        if (-not $ExpectedPublicKey -or $payload.public -ne $ExpectedPublicKey -or -not $payload.key -or -not $payload.password) { throw 'Backup does not match the configured release key.' }
        New-Item -ItemType Directory -Path $Directory -Force | Out-Null
        $protected = [Security.Cryptography.ProtectedData]::Protect([Text.Encoding]::UTF8.GetBytes($payload.password),$null,[Security.Cryptography.DataProtectionScope]::CurrentUser)
        [IO.File]::WriteAllBytes((Join-Path $Directory 'password.dpapi'),$protected)
        [IO.File]::WriteAllText((Join-Path $Directory 'fusion.key'),$payload.key,(New-Object Text.UTF8Encoding $false))
        [IO.File]::WriteAllText((Join-Path $Directory 'fusion.key.pub'),$payload.public,(New-Object Text.UTF8Encoding $false))
    } finally { if ($certificate) { $certificate.Dispose() }; $plainPassword=$null; $payload=$null }
}
if (-not $LibraryOnly) {
    if (-not $BackupPath) { throw 'Provide -BackupPath with an explicit destination on your offline backup drive, or the source backup when using -Restore.' }
    $Password = Read-Host 'Backup password (12+ characters; keep it separately)' -AsSecureString
    if (-not $Restore) {
        $confirmation = Read-Host 'Confirm backup password' -AsSecureString
        $first = (New-Object Management.Automation.PSCredential('backup',$Password)).GetNetworkCredential().Password
        $second = (New-Object Management.Automation.PSCredential('backup',$confirmation)).GetNetworkCredential().Password
        if (-not [string]::Equals($first,$second,[StringComparison]::Ordinal)) { throw 'Backup passwords do not match; no backup was written.' }
        $first=$null; $second=$null
    }
    $directory = Join-Path $env:USERPROFILE '.fusion-signing'
    if ($Restore) {
        $config = [IO.File]::ReadAllText((Join-Path (Split-Path -Parent $PSScriptRoot) 'src-tauri/tauri.conf.json')) | ConvertFrom-Json
        Restore-FusionSigningBackup $BackupPath $directory $Password $config.plugins.updater.pubkey
    } else { Export-FusionSigningBackup $directory $BackupPath $Password }
    Write-Output 'Signing recovery operation completed. No signing material was printed. Verify recovery before treating an offline backup as accepted.'
}
