$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Security
$project = Split-Path -Parent $PSScriptRoot
$signingDir = Join-Path $env:USERPROFILE '.fusion-signing'
$backupDir = Join-Path $env:USERPROFILE '.fusion-signing-backup'
$keyPath = Join-Path $signingDir 'fusion.key'
$passwordPath = Join-Path $signingDir 'password.dpapi'
New-Item -ItemType Directory -Path $signingDir -Force | Out-Null
if (-not (Test-Path -LiteralPath $keyPath)) {
    if (Test-Path -LiteralPath $passwordPath) { throw 'Signing files are incomplete. Restore the existing signing backup; do not rotate the key.' }
    $random = New-Object byte[] 48
    [Security.Cryptography.RandomNumberGenerator]::Create().GetBytes($random)
    $signingPassword = [Convert]::ToBase64String($random)
    $protected = [Security.Cryptography.ProtectedData]::Protect([Text.Encoding]::UTF8.GetBytes($signingPassword), $null, [Security.Cryptography.DataProtectionScope]::CurrentUser)
    [IO.File]::WriteAllBytes($passwordPath, $protected)
    Push-Location $project
    try {
        & (Join-Path $project 'node_modules/.bin/tauri.cmd') signer generate --ci --write-keys $keyPath --password $signingPassword *> $null
        if ($LASTEXITCODE -ne 0) { throw 'Could not generate update signing key.' }
    } finally { Pop-Location; $signingPassword = $null }
}
if (-not (Test-Path -LiteralPath $passwordPath)) { throw 'Restore password.dpapi from the signing backup.' }
$publicKey = [IO.File]::ReadAllText("$keyPath.pub").Trim()
$configPath = Join-Path $project 'src-tauri/tauri.conf.json'
$config = Get-Content -LiteralPath $configPath -Raw -Encoding UTF8 | ConvertFrom-Json
if ($config.plugins.updater.pubkey -and $config.plugins.updater.pubkey -ne $publicKey) { throw 'Configured public key differs. Restore the original signing material; automatic key rotation is forbidden.' }
$config.plugins.updater.pubkey = $publicKey
[IO.File]::WriteAllText($configPath, ($config | ConvertTo-Json -Depth 30) + "`n", (New-Object Text.UTF8Encoding $false))
New-Item -ItemType Directory -Path $backupDir -Force | Out-Null
foreach ($name in @('fusion.key','fusion.key.pub','password.dpapi')) {
    $destination = Join-Path $backupDir $name
    if (Test-Path -LiteralPath $destination) {
        if ((Get-FileHash -LiteralPath $destination).Hash -ne (Get-FileHash -LiteralPath (Join-Path $signingDir $name)).Hash) { throw 'Signing backup differs; nothing was overwritten.' }
    } else { Copy-Item -LiteralPath (Join-Path $signingDir $name) -Destination $destination }
}
Write-Output 'Update signing initialized. Private key is encrypted; its password is protected for this Windows user. Local backup created. Keep an independent offline recovery copy before publishing.'
