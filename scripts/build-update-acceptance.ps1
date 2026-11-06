param([string]$FromVersion = '0.1.30')
$ErrorActionPreference = 'Stop'
$project = Split-Path -Parent $PSScriptRoot
$targetVersion = (Get-Content -LiteralPath (Join-Path $project 'package.json') -Raw | ConvertFrom-Json).version
if ($FromVersion -notmatch '^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$' -or [version]$FromVersion -ge [version]$targetVersion) { throw 'FromVersion must be a stable version older than the target.' }
# A separate source tree and target directory keep acceptance features out of production artifacts.
$scratch = Join-Path ([IO.Path]::GetTempPath()) ('Fusion-update-acceptance-' + [guid]::NewGuid().ToString('N'))
[IO.Directory]::CreateDirectory($scratch) | Out-Null
$scratch = [IO.Path]::GetFullPath($scratch)
$files = & git -C $project ls-files --cached --others --exclude-standard
if ($LASTEXITCODE -ne 0) { throw 'Cannot enumerate workspace sources.' }
foreach ($relative in $files) {
    $source = [IO.Path]::GetFullPath((Join-Path $project $relative))
    $destination = [IO.Path]::GetFullPath((Join-Path $scratch $relative))
    if (-not $source.StartsWith($project + '\', [StringComparison]::OrdinalIgnoreCase) -or -not $destination.StartsWith($scratch + '\', [StringComparison]::OrdinalIgnoreCase)) { throw 'Source path escaped the workspace.' }
    if ([IO.File]::Exists($source)) {
        [IO.Directory]::CreateDirectory((Split-Path -Parent $destination)) | Out-Null
        [IO.File]::Copy($source, $destination)
    }
}
$utf8 = New-Object Text.UTF8Encoding($false)
foreach ($relative in @('package.json','package-lock.json','src-tauri/tauri.conf.json','src-tauri/Cargo.toml','crates/pool-core/Cargo.toml','Cargo.lock')) {
    $path = Join-Path $scratch $relative
    $content = [IO.File]::ReadAllText($path)
    if ($relative -eq 'Cargo.lock') {
        $pattern = '(?m)(^name = "(?:pool-core|personal-codex-pool)"\r?\nversion = ")' + [regex]::Escape($targetVersion) + '(")'
        $content = [regex]::Replace($content, $pattern, ('${1}' + $FromVersion + '${2}'))
    } elseif ($relative.EndsWith('.toml')) {
        $pattern = [regex]::new('(?m)^version = "' + [regex]::Escape($targetVersion) + '"')
        $content = $pattern.Replace($content, ('version = "' + $FromVersion + '"'), 1)
    } else {
        $pattern = [regex]::new('"version": "' + [regex]::Escape($targetVersion) + '"')
        $count = if ($relative -eq 'package-lock.json') { 2 } else { 1 }
        $content = $pattern.Replace($content, ('"version": "' + $FromVersion + '"'), $count)
    }
    [IO.File]::WriteAllText($path, $content, $utf8)
}
$chromePath = Join-Path $scratch 'src/WindowChrome.tsx'
$chrome = [IO.File]::ReadAllText($chromePath)
if (-not $chrome.Contains('Fusion {__APP_VERSION__}')) { throw 'Cannot label the offline acceptance UI.' }
[IO.File]::WriteAllText($chromePath, $chrome.Replace('Fusion {__APP_VERSION__}', 'Fusion {__APP_VERSION__} - OFFLINE ACCEPTANCE ONLY'), $utf8)
$configPath = Join-Path $scratch 'src-tauri/tauri.conf.json'
$config = [IO.File]::ReadAllText($configPath) | ConvertFrom-Json
$config.productName = 'Fusion'
$config.app.windows[0].title = 'Fusion - OFFLINE ACCEPTANCE ONLY'
$config.bundle.createUpdaterArtifacts = $false
[IO.File]::WriteAllText($configPath, ($config | ConvertTo-Json -Depth 30), $utf8)
# Vite/TypeScript use the already installed workspace dependencies; no package installation.
New-Item -ItemType Junction -Path (Join-Path $scratch 'node_modules') -Target (Join-Path $project 'node_modules') | Out-Null
$previousTarget = $env:CARGO_TARGET_DIR
try {
    $env:CARGO_TARGET_DIR = Join-Path $project ('target/update-acceptance/' + $FromVersion)
    Push-Location $scratch
    & (Join-Path $project 'node_modules/.bin/tauri.cmd') build --target x86_64-pc-windows-msvc --bundles nsis --features update-acceptance -- --locked
    if ($LASTEXITCODE -ne 0) { throw 'Offline acceptance build failed.' }
    Write-Output "Offline acceptance installer built under $env:CARGO_TARGET_DIR. It was NOT installed or uploaded."
    Write-Output "Temporary source tree retained at $scratch. Never distribute this build as a stable release."
} finally {
    Pop-Location
    if ($null -eq $previousTarget) { Remove-Item Env:CARGO_TARGET_DIR -ErrorAction SilentlyContinue } else { $env:CARGO_TARGET_DIR = $previousTarget }
}
