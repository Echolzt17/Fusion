$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Security
$project = Split-Path -Parent $PSScriptRoot
$signingDir = Join-Path $env:USERPROFILE '.fusion-signing'
$keyPath = Join-Path $signingDir 'fusion.key'
if (-not (Test-Path -LiteralPath $keyPath)) { throw 'Run npm run updates:setup before building the bootstrap installer.' }
$config = Get-Content -LiteralPath (Join-Path $project 'src-tauri/tauri.conf.json') -Raw -Encoding UTF8 | ConvertFrom-Json
if ($config.plugins.updater.pubkey -ne [IO.File]::ReadAllText("$keyPath.pub").Trim()) { throw 'Update signing key does not match the embedded public key.' }
try {
    $env:TAURI_SIGNING_PRIVATE_KEY = $keyPath
    $env:TAURI_SIGNING_PRIVATE_KEY_PASSWORD = [Text.Encoding]::UTF8.GetString([Security.Cryptography.ProtectedData]::Unprotect([IO.File]::ReadAllBytes((Join-Path $signingDir 'password.dpapi')), $null, [Security.Cryptography.DataProtectionScope]::CurrentUser))
    & node (Join-Path $project 'scripts/generate-notices.mjs')
    if ($LASTEXITCODE -ne 0) { throw 'License generation failed.' }
    & (Join-Path $project 'node_modules/.bin/tauri.cmd') build --target x86_64-pc-windows-msvc --bundles nsis -- --locked
    if ($LASTEXITCODE -ne 0) { throw 'Signed installer build failed.' }
} finally {
    Remove-Item Env:TAURI_SIGNING_PRIVATE_KEY -ErrorAction SilentlyContinue
    Remove-Item Env:TAURI_SIGNING_PRIVATE_KEY_PASSWORD -ErrorAction SilentlyContinue
}
