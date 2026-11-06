# Windows-only publisher storage. Never write a plaintext token or return it to logs.
Add-Type -AssemblyName System.Security

function Read-FusionGitCredential {
    # Reuse only the explicitly named owner's existing Git credential. Disable
    # interactive login and capture helper output inside this process only.
    $process = $null
    $output = $null
    try {
        $start = New-Object Diagnostics.ProcessStartInfo
        $start.FileName = 'git'
        $start.Arguments = 'credential-manager get'
        $start.UseShellExecute = $false
        $start.CreateNoWindow = $true
        $start.RedirectStandardInput = $true
        $start.RedirectStandardOutput = $true
        $start.RedirectStandardError = $true
        $start.EnvironmentVariables['GIT_TERMINAL_PROMPT'] = '0'
        $start.EnvironmentVariables['GCM_INTERACTIVE'] = 'never'
        $process = [Diagnostics.Process]::Start($start)
        $stdout = $process.StandardOutput.ReadToEndAsync()
        $stderr = $process.StandardError.ReadToEndAsync()
        $process.StandardInput.Write("protocol=https`nhost=github.com`nusername=Echolzt17`n`n")
        $process.StandardInput.Close()
        if (-not $process.WaitForExit(30000)) { $process.Kill(); return $null }
        if ($process.ExitCode -ne 0) { return $null }
        $output = $stdout.GetAwaiter().GetResult()
        $stderr.GetAwaiter().GetResult() | Out-Null
        $username = $null
        $token = $null
        foreach ($line in ($output -split '\r?\n')) {
            if ($line.StartsWith('username=')) { $username = $line.Substring(9) }
            if ($line.StartsWith('password=')) { $token = $line.Substring(9) }
        }
        if ($username -ne 'Echolzt17' -or $token -notmatch '^(github_pat_|gho_|ghu_|ghp_)[A-Za-z0-9_]+$') { return $null }
        return $token
    } catch {
        return $null
    } finally {
        $output = $null
        if ($null -ne $process) { $process.Dispose() }
    }
}

function Read-FusionPublishToken([string]$Path) {
    if (-not [IO.File]::Exists($Path)) { return $null }
    $clear = $null
    try {
        if ((Get-Item -LiteralPath $Path).Length -gt 16384) { throw 'Invalid credential size' }
        $entropy = [Text.Encoding]::UTF8.GetBytes('Fusion GitHub publisher/Echolzt17/Fusion/v1')
        $clear = [Security.Cryptography.ProtectedData]::Unprotect([IO.File]::ReadAllBytes($Path), $entropy, [Security.Cryptography.DataProtectionScope]::CurrentUser)
        $token = [Text.Encoding]::UTF8.GetString($clear)
        if ($token -notmatch '^(github_pat_|gho_|ghu_|ghp_)[A-Za-z0-9_]+$') { throw 'Invalid credential format' }
        return $token
    } catch {
        throw 'Saved publisher credential is unavailable. Enter a replacement in the local masked window.'
    } finally {
        if ($null -ne $clear) { [Array]::Clear($clear, 0, $clear.Length) }
    }
}

function Save-FusionPublishToken([string]$Path, [string]$Token) {
    $clear = $null
    $temporary = $null
    try {
        if ($Token.Length -gt 4096 -or $Token -notmatch '^(github_pat_|gho_|ghu_|ghp_)[A-Za-z0-9_]+$') { throw 'Invalid credential format' }
        $entropy = [Text.Encoding]::UTF8.GetBytes('Fusion GitHub publisher/Echolzt17/Fusion/v1')
        $clear = [Text.Encoding]::UTF8.GetBytes($Token)
        $encrypted = [Security.Cryptography.ProtectedData]::Protect($clear, $entropy, [Security.Cryptography.DataProtectionScope]::CurrentUser)
        [IO.Directory]::CreateDirectory([IO.Path]::GetDirectoryName($Path)) | Out-Null
        $temporary = $Path + '.' + [Guid]::NewGuid().ToString('N') + '.tmp'
        $stream = [IO.File]::Open($temporary, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::None)
        try { $stream.Write($encrypted, 0, $encrypted.Length); $stream.Flush($true) } finally { $stream.Dispose() }
        if ([IO.File]::Exists($Path)) { [IO.File]::Replace($temporary, $Path, [NullString]::Value) }
        else { [IO.File]::Move($temporary, $Path) }
    } catch {
        throw 'Unable to save encrypted publisher credential. No plaintext token was written.'
    } finally {
        if ($null -ne $clear) { [Array]::Clear($clear, 0, $clear.Length) }
        if ($temporary -and [IO.File]::Exists($temporary)) { [IO.File]::Delete($temporary) }
    }
}
