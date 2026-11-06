param([switch]$ValidateOnly, [switch]$UpdateTest, [string]$RepairVersion, [switch]$ReplaceToken)
$ErrorActionPreference = 'Stop'
$project = Split-Path -Parent $PSScriptRoot
$package = [IO.File]::ReadAllText((Join-Path $project 'package.json')) | ConvertFrom-Json
$version = $package.version
if ($version -notmatch '^\d+\.\d+\.\d+$') { throw 'Invalid project version.' }
$release = Join-Path $project 'target/x86_64-pc-windows-msvc/release'
$prepared = Join-Path $release ('publish/v' + $version)
$installer = Join-Path $prepared ('Fusion_' + $version + '_x64-setup.exe')
foreach ($path in @($installer, "$installer.sig", (Join-Path $prepared 'manifest.json'))) {
    if (-not [IO.File]::Exists($path)) { throw 'Prepared release is incomplete. Run the release preparation first.' }
}
& (Join-Path $release 'personal-codex-pool.exe') --verify-update-package (Join-Path $prepared 'manifest.json') $installer | Out-Null
if ($LASTEXITCODE -ne 0) { throw 'Prepared package signature failed. Nothing was uploaded.' }
$acceptance = Join-Path $prepared 'acceptance.json'
$node = (Get-Command node -ErrorAction Stop).Source
if ($UpdateTest) {
    if ($RepairVersion -notmatch '^\d+\.\d+\.\d+$' -or $RepairVersion -eq $version) { throw 'Specify the installed older -RepairVersion for an online update test.' }
    $repairFolder = Join-Path $release ('publish/v' + $RepairVersion)
    $repairInstaller = Join-Path $repairFolder ('Fusion_' + $RepairVersion + '_x64-setup.exe')
    foreach ($path in @($repairInstaller, "$repairInstaller.sig", (Join-Path $repairFolder 'manifest.json'))) {
        if (-not [IO.File]::Exists($path)) { throw 'Installed-version repair materials are incomplete. No upload was started.' }
    }
    & (Join-Path $release 'personal-codex-pool.exe') --verify-update-package (Join-Path $repairFolder 'manifest.json') $repairInstaller | Out-Null
    if ($LASTEXITCODE -ne 0) { throw 'Installed-version repair signature failed. Nothing was uploaded.' }
    Write-Output 'Explicit online update test: actual acceptance remains pending; no acceptance record is changed.'
} else {
    & $node --input-type=module -e "import {readFileSync} from 'node:fs'; import {pathToFileURL} from 'node:url'; const {checkAcceptance}=await import(pathToFileURL(process.argv[3])); const p=JSON.parse(readFileSync(process.argv[1],'utf8')); checkAcceptance(p,process.argv[2]);" $acceptance $version (Join-Path $PSScriptRoot 'release-update.mjs')
    if ($LASTEXITCODE -ne 0) { throw 'Real offline acceptance is incomplete. No authorization dialog or upload was started.' }
}
if ($ValidateOnly) { Write-Output 'Publisher: prepared signed packages verified; no dialog, credentials or network used.'; exit 0 }

. (Join-Path $PSScriptRoot 'publish-credential.ps1')
$credentialPath = Join-Path $env:LOCALAPPDATA 'FusionPublisher/github-token.dpapi'
$savedToken = $null
if (-not $ReplaceToken) {
    try { $savedToken = Read-FusionPublishToken $credentialPath }
    catch { Write-Output 'Saved publisher credential could not be loaded; opening the replacement window.' }
    if (-not $savedToken) {
        $savedToken = Read-FusionGitCredential
        if ($savedToken) { Write-Output 'Reusing the existing Echolzt17 Git Credential Manager authorization; no new token is needed.' }
    }
}
$form = $null
$tokenBox = $null
try {
if (-not $savedToken) {
Add-Type -AssemblyName System.Windows.Forms
Add-Type -AssemblyName System.Drawing
$form = New-Object Windows.Forms.Form
$form.Text = if ($UpdateTest) { 'Fusion 更新发布：加密保存授权' } else { 'Fusion 正式发布：加密保存授权' }
$form.ClientSize = New-Object Drawing.Size(590,255)
$form.StartPosition = 'CenterScreen'
$form.FormBorderStyle = 'FixedDialog'
$form.MaximizeBox = $false
$form.MinimizeBox = $false
$form.Font = New-Object Drawing.Font('Segoe UI',10)
$description = New-Object Windows.Forms.Label
$description.Location = New-Object Drawing.Point(20,18)
$description.Size = New-Object Drawing.Size(550,65)
$releaseLabel = if ($UpdateTest) { "online update test; repair v$RepairVersion first" } else { 'stable release' }
$description.Text = "仓库：Echolzt17/Fusion   目标版本：v$version`r`n验证身份后，将用当前 Windows 用户的 DPAPI 加密保存。`r`n后续发布自动复用；不会写入源码、安装包或日志。"
$form.Controls.Add($description)
$label = New-Object Windows.Forms.Label
$label.Location = New-Object Drawing.Point(20,95)
$label.Size = New-Object Drawing.Size(550,25)
$label.Text = '发布令牌：仅选择 Fusion 仓库，Contents 权限设为读写'
$form.Controls.Add($label)
$tokenBox = New-Object Windows.Forms.TextBox
$tokenBox.Location = New-Object Drawing.Point(20,126)
$tokenBox.Size = New-Object Drawing.Size(550,27)
$tokenBox.UseSystemPasswordChar = $true
$form.Controls.Add($tokenBox)
$send = New-Object Windows.Forms.Button
$send.Location = New-Object Drawing.Point(250,185)
$send.Size = New-Object Drawing.Size(205,35)
$send.Text = '加密保存并上传'
$send.DialogResult = [Windows.Forms.DialogResult]::OK
$send.Enabled = $false
$tokenBox.Add_TextChanged({$send.Enabled = $tokenBox.Text.Trim().Length -gt 20})
$form.Controls.Add($send)
$cancel = New-Object Windows.Forms.Button
$cancel.Location = New-Object Drawing.Point(465,185)
$cancel.Size = New-Object Drawing.Size(105,35)
$cancel.Text = '取消'
$cancel.DialogResult = [Windows.Forms.DialogResult]::Cancel
$form.Controls.Add($cancel)
$form.AcceptButton = $send
$form.CancelButton = $cancel
$form.Add_Shown({$form.Activate(); $tokenBox.Focus()})
Write-Output 'Waiting for one-time authorization in the local masked window; the verified credential will be encrypted for reuse.'
    if ($form.ShowDialog() -ne [Windows.Forms.DialogResult]::OK) {
        Write-Output 'Authorization canceled. No files were uploaded.'
        exit 2
    }
    $savedToken = $tokenBox.Text.Trim()
    $tokenBox.Clear()
    $form.Close()
} else {
    Write-Output 'Using the publisher credential protected for the current Windows user; no token dialog is needed.'
}
    $env:FUSION_GITHUB_TOKEN = $savedToken
    $savedToken = $null
    if ($env:FUSION_GITHUB_TOKEN -notmatch '^(github_pat_|gho_|ghu_|ghp_)[A-Za-z0-9_]+$') { throw 'Invalid GitHub credential format.' }
    $node = (Get-Command node -ErrorAction Stop).Source
    & $node (Join-Path $PSScriptRoot 'release-update.mjs') authorize
    if ($LASTEXITCODE -ne 0) { throw 'Publisher authorization failed; no new credential was saved. Use -ReplaceToken to supply a valid replacement.' }
    Save-FusionPublishToken $credentialPath $env:FUSION_GITHUB_TOKEN
    Write-Output 'Verified publisher credential saved with Windows CurrentUser DPAPI; future publications can reuse it.'
    if ($UpdateTest) {
        Write-Output 'Publishing the explicitly requested online test; actual acceptance stays pending. Feeds change only after public verification.'
        & $node (Join-Path $PSScriptRoot 'release-update.mjs') publish-test --version $version --repair-version $RepairVersion
    } else {
        Write-Output 'Publishing the accepted stable release to Echolzt17/Fusion. Feeds change after public verification.'
        & $node (Join-Path $PSScriptRoot 'release-update.mjs') initialize
        if ($LASTEXITCODE -ne 0) { throw 'Repository initialization failed. Review the account and token permissions, then retry.' }
        & $node (Join-Path $PSScriptRoot 'release-update.mjs') publish --version $version
    }
    if ($LASTEXITCODE -ne 0) { throw 'Stable publication did not complete. Check the release result before retrying; published files are not replaced.' }
    Write-Output "Published: https://github.com/Echolzt17/Fusion/releases/tag/v$version"
} catch {
    # Do not serialize exception objects or request headers from credential-handling code.
    Write-Output 'Publication stopped. A verified credential may remain encrypted for retry; no plaintext token was saved or printed. Check the non-secret status above.'
    exit 1
} finally {
    Remove-Item Env:FUSION_GITHUB_TOKEN -ErrorAction SilentlyContinue
    $savedToken = $null
    if ($null -ne $tokenBox) { $tokenBox.Clear() }
    if ($null -ne $form) { $form.Dispose() }
}
