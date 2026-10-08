<#
.SYNOPSIS
Installs the oyakata binary from GitHub Releases. No Rust toolchain needed.

.DESCRIPTION
Downloads the Windows archive for this machine's CPU, verifies its SHA-256, puts
oyakata.exe in the install directory and adds that directory to the user PATH.

  powershell -NoProfile -ExecutionPolicy Bypass -File scripts\install.ps1 [-Version v0.4.0] [-InstallDir C:\tools\oyakata] [-NoPath]
  irm https://raw.githubusercontent.com/ShintaroOba/oyakata/main/scripts/install.ps1 | iex

Environment variables (overridden by the parameters):
  OYAKATA_INSTALL_DIR  where oyakata.exe goes (default: %LOCALAPPDATA%\Programs\oyakata)
  OYAKATA_VERSION      release tag to install (default: latest)
  OYAKATA_REPO         GitHub repository (default: ShintaroOba/oyakata)
  OYAKATA_BASE_URL     download the archive from here instead of GitHub (a mirror or file:/// URL)
  OYAKATA_TARGET       force a target triple instead of detecting it (e.g. x86_64-pc-windows-msvc)

The download goes through the system proxy settings (Invoke-WebRequest).
#>
[CmdletBinding()]
param(
    [string]$Version = $(if ($env:OYAKATA_VERSION) { $env:OYAKATA_VERSION } else { 'latest' }),
    [string]$InstallDir = $(if ($env:OYAKATA_INSTALL_DIR) { $env:OYAKATA_INSTALL_DIR } else { Join-Path $env:LOCALAPPDATA 'Programs\oyakata' }),
    [string]$Repo = $(if ($env:OYAKATA_REPO) { $env:OYAKATA_REPO } else { 'ShintaroOba/oyakata' }),
    [switch]$NoPath
)

$ErrorActionPreference = 'Stop'
try {
    [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12
} catch {}

if ($env:OYAKATA_TARGET) {
    $target = $env:OYAKATA_TARGET
} else {
    $arch = $env:PROCESSOR_ARCHITEW6432
    if (-not $arch) { $arch = $env:PROCESSOR_ARCHITECTURE }
    switch ($arch) {
        'AMD64' { $target = 'x86_64-pc-windows-msvc' }
        'ARM64' { $target = 'aarch64-pc-windows-msvc' }
        default { throw "oyakata install: unsupported CPU architecture: $arch" }
    }
}
$asset = "oyakata-$target.zip"

if ($env:OYAKATA_BASE_URL) {
    $base = $env:OYAKATA_BASE_URL.TrimEnd('/')
} elseif ($Version -eq 'latest') {
    $base = "https://github.com/$Repo/releases/latest/download"
} else {
    $base = "https://github.com/$Repo/releases/download/$Version"
}

$tmp = Join-Path ([IO.Path]::GetTempPath()) ('oyakata-install-' + [Guid]::NewGuid().ToString('n'))
New-Item -ItemType Directory -Path $tmp | Out-Null
try {
    $zip = Join-Path $tmp $asset
    Write-Host "Downloading $base/$asset"
    try {
        Invoke-WebRequest -Uri "$base/$asset" -OutFile $zip -UseBasicParsing
    } catch {
        throw "oyakata install: download failed (no release for $target at $base?): $($_.Exception.Message)"
    }

    $sumFile = "$zip.sha256"
    $haveSum = $true
    try {
        Invoke-WebRequest -Uri "$base/$asset.sha256" -OutFile $sumFile -UseBasicParsing
    } catch {
        $haveSum = $false
        Write-Warning "no checksum published for $asset, skipping verification"
    }
    if ($haveSum) {
        $expected = ((Get-Content $sumFile -Raw).Trim() -split '\s+')[0].ToLower()
        $actual = (Get-FileHash $zip -Algorithm SHA256).Hash.ToLower()
        if ($expected -ne $actual) {
            throw "oyakata install: checksum mismatch for $asset (expected $expected, got $actual)"
        }
    }

    $extract = Join-Path $tmp 'x'
    Expand-Archive -Path $zip -DestinationPath $extract -Force
    $exe = Get-ChildItem -Path $extract -Filter oyakata.exe -Recurse -File | Select-Object -First 1
    if (-not $exe) { throw 'oyakata install: the archive did not contain oyakata.exe' }

    New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null
    $InstallDir = (Resolve-Path $InstallDir).Path
    $dest = Join-Path $InstallDir 'oyakata.exe'
    $old = "$dest.old"
    # A running daemon locks its exe against overwrite but not against rename, so move the
    # old file aside, drop the new one in, and delete the old one when nothing holds it.
    Remove-Item $old -Force -ErrorAction SilentlyContinue
    if (Test-Path $dest) { Move-Item $dest $old -Force }
    Copy-Item $exe.FullName $dest -Force
    Remove-Item $old -Force -ErrorAction SilentlyContinue
} finally {
    Remove-Item $tmp -Recurse -Force -ErrorAction SilentlyContinue
}

function Test-OnPath([string]$list, [string]$dir) {
    $want = $dir.TrimEnd('\')
    foreach ($p in ($list -split ';')) {
        if ($p -and ($p.TrimEnd('\') -ieq $want)) { return $true }
    }
    return $false
}

if (-not $NoPath) {
    $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
    if (-not (Test-OnPath $userPath $InstallDir)) {
        $newPath = if ($userPath) { $userPath.TrimEnd(';') + ';' + $InstallDir } else { $InstallDir }
        [Environment]::SetEnvironmentVariable('Path', $newPath, 'User')
        Write-Host "Added $InstallDir to your user PATH. Open a new terminal to pick it up."
    }
    if (-not (Test-OnPath $env:Path $InstallDir)) { $env:Path = "$InstallDir;$env:Path" }
}

& $dest --version
Write-Host "Installed: $dest"

$other = Get-Command oyakata -ErrorAction SilentlyContinue | Where-Object { $_.Source -and ($_.Source -ne $dest) } | Select-Object -First 1
if ($other) {
    Write-Warning "``oyakata`` currently resolves to $($other.Source), which comes earlier on PATH."
}
# `oyakata status` exits 0 either way; it prints "running: ..." or "not running on ...".
$state = & $dest status 2>$null | Select-Object -First 1
if ("$state" -like 'running*') {
    Write-Warning 'A daemon from the previous version is still running. Restart it with: oyakata stop; oyakata'
}
