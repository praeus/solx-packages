#Requires -Version 7
<#
.SYNOPSIS
    Build both halves of solx-prompt and stage the two artifacts install.solx reads.

.DESCRIPTION
    install.solx reads crate/bin/solx-prompt.wasm and widget/dist/solx-prompt.js,
    so both must be staged before installing. Staging the wasm into crate/bin/
    rather than referencing crate/target/ keeps the path stable under
    CARGO_TARGET_DIR.

.PARAMETER Install
    Also run `solx install-package .` once both artifacts are staged.

.PARAMETER SkipRust
    Skip the cargo half. This package's two build costs are very different -
    iterating on the widget should not pay for a release LTO wasm build.

.PARAMETER SkipWidget
    Skip the vite half.
#>
param([switch]$Install, [switch]$SkipRust, [switch]$SkipWidget)

$ErrorActionPreference = "Stop"
Set-Location $PSScriptRoot

# Rust first: its only prerequisite failure is instant, so failing here costs
# nothing, where failing after a vite build would have wasted it.
if (-not $SkipRust) {
    if (-not (rustup target list --installed | Select-String -SimpleMatch 'wasm32-wasip2')) {
        throw "wasm32-wasip2 target is not installed. Run: rustup target add wasm32-wasip2"
    }

    # Push-Location, not --manifest-path: cargo discovers .cargo/config.toml from
    # the cwd, and crate/.cargo/config.toml is where `cargo wasm` is defined.
    Push-Location crate
    try {
        cargo build --release --target wasm32-wasip2
        if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }
    } finally { Pop-Location }

    # cargo emits the crate name with underscores on every platform.
    $src = "crate/target/wasm32-wasip2/release/solx_prompt.wasm"
    if (-not (Test-Path $src)) { throw "build produced no artifact at $src" }

    New-Item -ItemType Directory -Force -Path crate/bin | Out-Null
    Copy-Item $src crate/bin/solx-prompt.wasm -Force
    Write-Host "staged crate/bin/solx-prompt.wasm ($((Get-Item crate/bin/solx-prompt.wasm).Length) bytes)"
}

if (-not $SkipWidget) {
    Push-Location widget
    try {
        # npm ci, not install: package-lock.json is committed.
        if (-not (Test-Path node_modules)) {
            npm ci
            if ($LASTEXITCODE -ne 0) { throw "npm ci failed" }
        }
        npm run build
        if ($LASTEXITCODE -ne 0) { throw "vite build failed" }
    } finally { Pop-Location }

    $out = "widget/dist/solx-prompt.js"
    if (-not (Test-Path $out)) { throw "vite produced no artifact at $out" }
    Write-Host "built $out ($((Get-Item $out).Length) bytes)"
}

if ($Install) {
    solx install-package .
    if ($LASTEXITCODE -ne 0) { throw "solx install-package failed" }
}
