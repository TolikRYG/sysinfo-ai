[CmdletBinding()]
param(
    [switch]$SkipTests,
    [switch]$Clippy
)

$ErrorActionPreference = 'Stop'
if ($env:OS -ne 'Windows_NT') {
    throw 'This script builds on Windows with the MSVC toolchain.'
}
if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    throw 'Cargo was not found. Install Rust with rustup and reopen PowerShell.'
}

function Invoke-Cargo {
    param([Parameter(Mandatory = $true)][string[]]$CargoArgs)
    & cargo @CargoArgs
    if ($LASTEXITCODE -ne 0) {
        throw "cargo $($CargoArgs -join ' ') failed (exit $LASTEXITCODE)."
    }
}

Push-Location -LiteralPath $PSScriptRoot
try {
    # The original CLI lockfile does not describe this GUI dependency graph.
    # Generate once; retain/commit the new lockfile for subsequent locked builds.
    if (-not (Test-Path -LiteralPath 'Cargo.lock')) {
        Invoke-Cargo -CargoArgs @('generate-lockfile')
    }
    Invoke-Cargo -CargoArgs @('fmt', '--all')
    Invoke-Cargo -CargoArgs @('check', '--locked', '--all-targets')
    if (-not $SkipTests) {
        Invoke-Cargo -CargoArgs @('test', '--locked', '--all-targets')
    }
    if ($Clippy) {
        Invoke-Cargo -CargoArgs @('clippy', '--locked', '--all-targets')
    }
    Invoke-Cargo -CargoArgs @('build', '--locked', '--release', '--target', 'x86_64-pc-windows-msvc')
    $exe = Join-Path $PSScriptRoot 'target\x86_64-pc-windows-msvc\release\sysinfo-ai.exe'
    if (-not (Test-Path -LiteralPath $exe)) {
        throw 'Cargo completed, but the expected executable was not found.'
    }
    $dist = Join-Path $PSScriptRoot 'dist'
    New-Item -ItemType Directory -Path $dist -Force | Out-Null
    $destination = Join-Path $dist 'sysinfo-ai.exe'
    Copy-Item -LiteralPath $exe -Destination $destination -Force
    $hash = (Get-FileHash -LiteralPath $destination -Algorithm SHA256).Hash
    $bytes = (Get-Item -LiteralPath $destination).Length
    Write-Host "Built: $destination"
    Write-Host "Size: $bytes bytes"
    Write-Host "SHA256: $hash"
    Write-Host 'Only sysinfo-ai.exe is needed for distribution; do not include your reports or API key.'
}
finally {
    Pop-Location
}
