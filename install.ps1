<#
install.ps1 - Build lean-ctx locally on Windows and install it into Cargo's bin directory.

Usage:
    .\install.ps1
    .\install.ps1 -BuildOnly
    .\install.ps1 -NoCockpit

After the engine is installed, the script adds the free LeanCTX Cockpit from the
GitHub release matching the engine version: it downloads
lean-ctx-cockpit-x86_64-pc-windows-msvc.tar.gz and COCKPIT_SHA256SUMS, refuses
the Cockpit when the checksum is missing or differs, and runs
`lean-ctx cockpit install --from <dir>`, which verifies its signatures offline.
A Cockpit problem never fails the engine install. Skip it with -NoCockpit or
LEAN_CTX_NO_COCKPIT=1. The Cockpit binary is licensed under the LeanCTX Cockpit
EULA (free to use, including commercially); the engine is Apache-2.0.
#>

param(
    [switch]$BuildOnly,
    [switch]$NoCockpit,
    [switch]$Help
)

$ErrorActionPreference = 'Stop'

if ($Help) {
    Write-Host 'Usage: .\install.ps1 [-BuildOnly] [-NoCockpit] [-Help]'
    Write-Host ''
    Write-Host '  (no args)    Build lean-ctx locally and install it into Cargo''s bin directory'
    Write-Host '  -BuildOnly   Build only, do not install'
    Write-Host '  -NoCockpit   Do not add the LeanCTX Cockpit (also: LEAN_CTX_NO_COCKPIT=1)'
    Write-Host '  -Help        Show this help message'
    exit 0
}

$Repo = 'yvgude/lean-ctx'

# Adds the release's Cockpit bundle to the installed engine. Every failure is
# reported in one line and swallowed: the engine install stays successful.
function Install-Cockpit {
    param([string]$Engine)

    if ($NoCockpit -or $env:LEAN_CTX_NO_COCKPIT -eq '1') {
        Write-Host 'Cockpit: skipped (-NoCockpit / LEAN_CTX_NO_COCKPIT=1).'
        return
    }
    $work = Join-Path ([System.IO.Path]::GetTempPath()) ('lean-ctx-cockpit-' + [guid]::NewGuid().ToString('N'))
    try {
        # Native tools may write to stderr; only explicit checks decide.
        $ErrorActionPreference = 'Continue'
        if ($env:PROCESSOR_ARCHITECTURE -ne 'AMD64') {
            throw "no Cockpit build for $($env:PROCESSOR_ARCHITECTURE)"
        }
        $versionLine = (& $Engine --version) | Select-Object -First 1
        if ($versionLine -notmatch '^lean-ctx (\d+\.\d+\.\d+)') {
            throw 'could not read the engine version'
        }
        $asset = 'lean-ctx-cockpit-x86_64-pc-windows-msvc.tar.gz'
        $base = "https://github.com/$Repo/releases/download/v$($Matches[1])"
        New-Item -ItemType Directory -Path (Join-Path $work 'extract') -Force -ErrorAction Stop | Out-Null
        $archive = Join-Path $work $asset
        $sums = Join-Path $work 'COCKPIT_SHA256SUMS'
        Write-Host 'Downloading Cockpit...'
        try {
            $ProgressPreference = 'SilentlyContinue'
            Invoke-WebRequest -UseBasicParsing -Uri "$base/$asset" -OutFile $archive -TimeoutSec 120 -ErrorAction Stop
            Invoke-WebRequest -UseBasicParsing -Uri "$base/COCKPIT_SHA256SUMS" -OutFile $sums -TimeoutSec 30 -ErrorAction Stop
        }
        catch {
            throw 'download failed'
        }
        $expected = $null
        foreach ($line in Get-Content -LiteralPath $sums -ErrorAction Stop) {
            $fields = $line -split '\s+', 2
            if ($fields.Count -eq 2 -and ($fields[1] -eq $asset -or $fields[1] -eq "*$asset")) {
                $expected = $fields[0].ToLowerInvariant()
                break
            }
        }
        $actual = (Get-FileHash -LiteralPath $archive -Algorithm SHA256 -ErrorAction Stop).Hash.ToLowerInvariant()
        if (-not $expected -or $actual -ne $expected) {
            throw 'checksum missing or mismatched - refusing an unverified Cockpit'
        }
        & tar.exe -xzf $archive -C (Join-Path $work 'extract') 2>$null
        $bundle = Join-Path $work 'extract\lean-ctx-cockpit'
        if ($LASTEXITCODE -ne 0 -or -not (Test-Path -LiteralPath (Join-Path $bundle 'manifest.json') -PathType Leaf)) {
            throw 'the bundle could not be unpacked'
        }
        $errorFile = Join-Path $work 'install.err'
        & $Engine cockpit install --from $bundle 2>$errorFile | Out-Null
        if ($LASTEXITCODE -ne 0) {
            $reason = Get-Content -LiteralPath $errorFile -ErrorAction SilentlyContinue | Select-Object -First 1
            if (-not $reason) { $reason = 'verification failed' }
            throw $reason
        }
        Write-Host 'Cockpit: installed and verified - open it with: lean-ctx cockpit'
    }
    catch {
        Write-Host "Cockpit: not installed ($($_.Exception.Message)); the engine works without it."
    }
    finally {
        Remove-Item -LiteralPath $work -Recurse -Force -ErrorAction SilentlyContinue
    }
}

function Get-CargoBinDir {
    if ($env:CARGO_HOME) {
        return Join-Path $env:CARGO_HOME 'bin'
    }

    return Join-Path $HOME '.cargo\bin'
}

function Stop-RunningLeanCtx {
    $existing = Get-Command lean-ctx -ErrorAction SilentlyContinue
    if ($null -ne $existing) {
        Write-Host 'Stopping running lean-ctx (if any)...'
        try {
            & $existing.Source stop | Out-Null
        }
        catch {
        }
    }
}

$scriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$rustDir = Join-Path $scriptDir 'rust'

if (-not (Test-Path $rustDir -PathType Container)) {
    throw "Rust project not found at $rustDir"
}

if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    throw 'cargo not found. Install Rust from https://rustup.rs/'
}

$cargoBinDir = Get-CargoBinDir
$builtBinary = Join-Path $rustDir 'target\release\lean-ctx.exe'
$installedBinary = Join-Path $cargoBinDir 'lean-ctx.exe'

Write-Host 'lean-ctx Windows installer'
Write-Host '━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━'
Write-Host 'Mode: build from source'
Write-Host ''
Write-Host 'Building lean-ctx (release)...'

Push-Location $rustDir
try {
    & cargo build --release
}
finally {
    Pop-Location
}

if (-not (Test-Path $builtBinary -PathType Leaf)) {
    throw "Build failed - binary not found at $builtBinary"
}

Write-Host "Built: $builtBinary"

if ($BuildOnly) {
    Write-Host 'Done (build only).'
    exit 0
}

New-Item -ItemType Directory -Path $cargoBinDir -Force | Out-Null

$tempBinary = Join-Path $cargoBinDir ('.lean-ctx.new.' + $PID + '.exe')
Copy-Item -Path $builtBinary -Destination $tempBinary -Force
Stop-RunningLeanCtx
Move-Item -Path $tempBinary -Destination $installedBinary -Force

Write-Host "Installed: $installedBinary"

Install-Cockpit -Engine $installedBinary

$pathEntries = @($env:Path -split ';' | Where-Object { $_ })
if ($pathEntries -notcontains $cargoBinDir) {
    Write-Host ''
    Write-Warning "$cargoBinDir is not in your PATH."
    Write-Host 'Add it to your user PATH, then restart your shell.'
}

Write-Host ''
Write-Host 'Done! Verify with: lean-ctx --version'