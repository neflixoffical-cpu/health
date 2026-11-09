<#
    Health - release build.

    Result: ..\installer\HealthSetup.exe - one file, health.exe is packed inside.
    Needs:  Rust (stable, MSVC toolchain): https://rustup.rs

    Run from this folder:
        powershell -ExecutionPolicy Bypass -File build.ps1
#>
$ErrorActionPreference = 'Stop'
$src = $PSScriptRoot
$root = Split-Path $src -Parent
$out = Join-Path $root 'installer'

if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    Write-Host 'Не найден Rust (cargo). Установите его с https://rustup.rs и запустите скрипт снова.' -ForegroundColor Red
    exit 1
}

# Короткая папка сборки: глубокие пути ломают link.exe (лимит MAX_PATH в 260 символов).
if (-not $env:CARGO_TARGET_DIR) {
    $env:CARGO_TARGET_DIR = Join-Path $env:LOCALAPPDATA 'health-build'
}
$target = $env:CARGO_TARGET_DIR
$sw = [Diagnostics.Stopwatch]::StartNew()

Push-Location $src
try {
    Write-Host '==> Собираю health.exe' -ForegroundColor Green
    cargo build --release --locked -p health
    if ($LASTEXITCODE -ne 0) { throw 'Сборка health.exe не удалась' }

    $env:HEALTH_PAYLOAD = Join-Path $target 'release\health.exe'
    Write-Host '==> Собираю установщик HealthSetup.exe' -ForegroundColor Green
    cargo build --release --locked -p health-setup
    if ($LASTEXITCODE -ne 0) { throw 'Сборка установщика не удалась' }
}
finally {
    Remove-Item Env:HEALTH_PAYLOAD -ErrorAction SilentlyContinue
    Pop-Location
}

New-Item -ItemType Directory -Force $out | Out-Null
$setup = Join-Path $out 'HealthSetup.exe'
Copy-Item (Join-Path $target 'release\health-setup.exe') $setup -Force
$mb = [math]::Round((Get-Item $setup).Length / 1MB, 1)
$sec = [math]::Round($sw.Elapsed.TotalSeconds)
Write-Host ''
Write-Host "Готово за $sec с: $setup ($mb МБ)" -ForegroundColor Green
Write-Host 'Запустите HealthSetup.exe, затем откройте новую консоль и введите: health'
