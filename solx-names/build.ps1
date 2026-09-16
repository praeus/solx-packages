#Requires -Version 7
<#
.SYNOPSIS
    Build the solx-names wasm component and stage it at bin/solx-names.wasm.

.DESCRIPTION
    install.solx reads bin/solx-names.wasm as its very first statement, so
    the artifact must be staged before installing. Staging into bin/ rather
    than referencing target/ keeps the path stable under CARGO_TARGET_DIR.

.PARAMETER Install
    Also run `solx install-package .` once the artifact is staged.
#>
param([switch]$Install)

$ErrorActionPreference = "Stop"
Set-Location $PSScriptRoot

if (-not (rustup target list --installed | Select-String -SimpleMatch 'wasm32-wasip2')) {
    throw "wasm32-wasip2 target is not installed. Run: rustup target add wasm32-wasip2"
}

cargo build --release --target wasm32-wasip2
if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }

$src = "target/wasm32-wasip2/release/solx_names.wasm"
if (-not (Test-Path $src)) { throw "build produced no artifact at $src" }

New-Item -ItemType Directory -Force -Path bin | Out-Null
Copy-Item $src bin/solx-names.wasm -Force
Write-Host "staged bin/solx-names.wasm ($((Get-Item bin/solx-names.wasm).Length) bytes)"

if ($Install) {
    solx install-package .
    if ($LASTEXITCODE -ne 0) { throw "solx install-package failed" }
}
