$ErrorActionPreference = 'Stop'
$Root = Split-Path -Parent $PSScriptRoot
$Binary = Join-Path $Root 'target\release\balatro-mcp.exe'
if (-not (Test-Path -LiteralPath $Binary)) {
    Push-Location -LiteralPath $Root
    try { cargo build --release } finally { Pop-Location }
}
$code = 1
Push-Location -LiteralPath $Root
try {
    & $Binary
    $code = $LASTEXITCODE
} finally {
    Pop-Location
}
exit $code
