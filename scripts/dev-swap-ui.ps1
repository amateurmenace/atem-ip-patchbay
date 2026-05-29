# dev-swap-ui.ps1 — instant UI iteration without GitHub Actions.
#
# Copies bmd_emulator\static\* into the installed ATEM IP Patchbay's
# resources\static\ directory so HTML / JS / CSS changes show up
# instantly when you press F5 in the WebView. No rebuild needed.
#
# Optional: -NetDiag also rebuilds tools\atem-net-diag\ locally (no
# libclang required) and copies the resulting .exe into the
# installed app's sidecar\ folder. atem-net-diag changes (like the
# alpha.43 multithreading fix) can be tested in ~2 min total.
#
# Run from the repo root:
#   .\scripts\dev-swap-ui.ps1
#   .\scripts\dev-swap-ui.ps1 -NetDiag
#   .\scripts\dev-swap-ui.ps1 -InstallDir "C:\Custom\Path\ATEM IP Patchbay"
#
# What the script does NOT touch:
#   - The main .exe / NDI/OMT DLLs / FFmpeg sidecar / libomt / libvmx.
#     Those need a real cargo tauri build (which needs libclang for
#     grafton-ndi's bindgen). Use CI for those, or install LLVM
#     portable + set LIBCLANG_PATH for local cargo tauri dev.
#   - State files under %APPDATA% / %LOCALAPPDATA%\ATEM IP Patchbay\
#     instances\. Your tile configs survive UI swaps untouched.
#
# Safety: the script REFUSES to run if the installed app is currently
# running (file lock would silently skip the .exe replace and you'd
# get a confusing "my changes aren't showing up" result). Close the
# app first; the script tells you which PIDs to kill.

[CmdletBinding()]
param(
    [string]$InstallDir = "",
    [switch]$NetDiag,
    [switch]$Force
)

$ErrorActionPreference = "Stop"

# ---------------------------------------------------------------
# Resolve install dir + locations
# ---------------------------------------------------------------
function Resolve-InstallDir {
    param([string]$Override)
    if ($Override) {
        if (-not (Test-Path $Override)) {
            throw "Override -InstallDir '$Override' does not exist."
        }
        return $Override
    }
    $candidates = @(
        (Join-Path $env:LOCALAPPDATA "ATEM IP Patchbay"),
        "C:\Program Files\ATEM IP Patchbay",
        "C:\Program Files (x86)\ATEM IP Patchbay"
    )
    foreach ($c in $candidates) {
        if (Test-Path (Join-Path $c "resources\static\index.html")) {
            return $c
        }
    }
    # Fall back to %LOCALAPPDATA% even if it doesn't exist yet — the
    # error message below will tell the user what to install.
    return (Join-Path $env:LOCALAPPDATA "ATEM IP Patchbay")
}

$repoRoot = (Get-Location).Path
$staticSrc = Join-Path $repoRoot "bmd_emulator\static"
if (-not (Test-Path (Join-Path $staticSrc "index.html"))) {
    Write-Host "ERROR: run this from the repo root. Couldn't find" -ForegroundColor Red
    Write-Host "       bmd_emulator\static\index.html relative to the cwd."
    exit 1
}

$installDir = Resolve-InstallDir -Override $InstallDir
$resourceDir = Join-Path $installDir "resources"
$staticDest = Join-Path $resourceDir "static"
$sidecarDest = Join-Path $resourceDir "sidecar"

if (-not (Test-Path $staticDest)) {
    Write-Host "ERROR: could not find an installed ATEM IP Patchbay at:" -ForegroundColor Red
    Write-Host "       $installDir"
    Write-Host ""
    Write-Host "Either install an alpha first (download .exe from the Releases" -ForegroundColor Yellow
    Write-Host "page) or pass -InstallDir explicitly:"
    Write-Host "    .\scripts\dev-swap-ui.ps1 -InstallDir 'C:\Custom\Path'"
    exit 1
}

# ---------------------------------------------------------------
# Safety: refuse if the app is running
# ---------------------------------------------------------------
$mainExe = "atem-ip-patchbay"
$running = Get-Process -Name $mainExe -ErrorAction SilentlyContinue
if ($running -and -not $Force) {
    Write-Host "ERROR: the app is currently running. File-locks would skip" -ForegroundColor Red
    Write-Host "       silent files and you'd see stale UI."
    Write-Host ""
    Write-Host "PIDs to close:"
    foreach ($p in $running) {
        Write-Host "    $($p.Id) - $($p.ProcessName)"
    }
    Write-Host ""
    Write-Host "Either close the app (Cmd-Q / × button), or re-run with -Force"
    Write-Host "(which will taskkill the running processes first)."
    exit 1
}
if ($running -and $Force) {
    Write-Host "Force-killing running atem-ip-patchbay processes…" -ForegroundColor Yellow
    foreach ($p in $running) {
        Stop-Process -Id $p.Id -Force -ErrorAction SilentlyContinue
    }
    Start-Sleep -Milliseconds 500
}
# Also kill bundled atem-net-diag if -NetDiag (its .exe is locked
# when the dashboard is open).
if ($NetDiag) {
    $diag = Get-Process -Name "atem-net-diag" -ErrorAction SilentlyContinue
    if ($diag) {
        Write-Host "Killing atem-net-diag processes (Net Utility was open)…" -ForegroundColor Yellow
        foreach ($p in $diag) {
            Stop-Process -Id $p.Id -Force -ErrorAction SilentlyContinue
        }
        Start-Sleep -Milliseconds 500
    }
}

# ---------------------------------------------------------------
# Swap the static files
# ---------------------------------------------------------------
Write-Host ""
Write-Host "Swapping UI from:" -ForegroundColor Cyan
Write-Host "    $staticSrc"
Write-Host "into:"
Write-Host "    $staticDest"
Write-Host ""

$copied = 0
$skipped = 0
Get-ChildItem -Path $staticSrc -File | ForEach-Object {
    $dest = Join-Path $staticDest $_.Name
    $srcHash = (Get-FileHash $_.FullName -Algorithm MD5).Hash
    if (Test-Path $dest) {
        $destHash = (Get-FileHash $dest -Algorithm MD5).Hash
        if ($srcHash -eq $destHash) {
            $skipped++
            return
        }
    }
    Copy-Item -Path $_.FullName -Destination $dest -Force
    $copied++
    Write-Host "    [copied] $($_.Name)" -ForegroundColor Green
}
Write-Host ""
Write-Host "$copied file(s) copied, $skipped file(s) unchanged." -ForegroundColor Cyan

# ---------------------------------------------------------------
# Optional: rebuild + swap atem-net-diag.exe
# ---------------------------------------------------------------
if ($NetDiag) {
    Write-Host ""
    Write-Host "Rebuilding atem-net-diag (no libclang needed)…" -ForegroundColor Cyan
    $netDiagDir = Join-Path $repoRoot "tools\atem-net-diag"
    if (-not (Test-Path (Join-Path $netDiagDir "Cargo.toml"))) {
        Write-Host "ERROR: tools\atem-net-diag\Cargo.toml not found." -ForegroundColor Red
        exit 1
    }
    Push-Location $netDiagDir
    try {
        $cargoExitCode = 0
        cargo build --release 2>&1 | ForEach-Object {
            Write-Host "    $_"
        }
        $cargoExitCode = $LASTEXITCODE
        if ($cargoExitCode -ne 0) {
            Write-Host "ERROR: cargo build failed (exit $cargoExitCode)." -ForegroundColor Red
            exit 1
        }
    } finally {
        Pop-Location
    }
    $diagExe = Join-Path $netDiagDir "target\release\atem-net-diag.exe"
    if (-not (Test-Path $diagExe)) {
        Write-Host "ERROR: built .exe not found at $diagExe" -ForegroundColor Red
        exit 1
    }
    $diagDest = Join-Path $sidecarDest "atem-net-diag.exe"
    Copy-Item -Path $diagExe -Destination $diagDest -Force
    $size = [math]::Round((Get-Item $diagDest).Length / 1MB, 2)
    Write-Host "    [copied] atem-net-diag.exe ($size MB)" -ForegroundColor Green
}

# ---------------------------------------------------------------
# Done
# ---------------------------------------------------------------
Write-Host ""
Write-Host "Done. Next steps:" -ForegroundColor Green
Write-Host "    1. Launch ATEM IP Patchbay (Start menu → ATEM IP Patchbay)"
Write-Host "    2. Press F5 in the WebView to reload static files."
if ($NetDiag) {
    Write-Host "    3. Click 'Net Utility' in the topbar to spawn the new"
    Write-Host "       atem-net-diag.exe."
}
Write-Host ""
Write-Host "If a change doesn't show:" -ForegroundColor Yellow
Write-Host "    - Right-click the WebView → 'Reload' (some keybinds don't"
Write-Host "      reach the WebView)."
Write-Host "    - Check Get-FileHash on the file in resources\static\ to"
Write-Host "      confirm the copy went through."
Write-Host ""
