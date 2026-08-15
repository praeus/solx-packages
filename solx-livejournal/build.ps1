# Build the solx-livejournal wasm component and stage it at bin/solx-livejournal.wasm.
#
# The component is JavaScript, compiled by solx-quickjs's
# `build-javascript-action` (componentize-qjs under the hood). That action must
# already be installed and its CLI built — see ../solx-quickjs/README.md.
#
# Usage: ./build.ps1 [-Install]
param([switch]$Install)

$ErrorActionPreference = 'Stop'
Set-Location $PSScriptRoot

$root = (Resolve-Path './src').Path -replace '\\', '/'
$json = @{
    action_name           = 'solx-livejournal'
    entry_artifact_name   = 'solx-livejournal.js'
    source_artifact_names = @('solx-livejournal.js')
    artifact_root         = $root
} | ConvertTo-Json -Compress

solx exec /packages/solx-quickjs/build-javascript-action --json $json

$src = 'src/solx-livejournal.js.wasm'
if (-not (Test-Path $src)) { throw "build produced no artifact at $src" }

New-Item -ItemType Directory -Force bin | Out-Null
Copy-Item $src bin/solx-livejournal.wasm -Force
"staged bin/solx-livejournal.wasm ($((Get-Item bin/solx-livejournal.wasm).Length) bytes)"

if ($Install) { solx install-package . }
