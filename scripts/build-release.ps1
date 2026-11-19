# Construit Tempo en release, en local : installe les dépendances de l'interface si besoin,
# télécharge les outils de conversion manquants (scripts/telecharger-outils.ps1), puis lance
# `cargo tauri build` (qui compile l'interface via beforeBuildCommand, puis l'application).
#
# Usage : powershell -ExecutionPolicy Bypass -File scripts/build-release.ps1 [options]
#   -Bundles msi,nsis   installeurs à produire (défaut : msi ; NSIS est souvent bloqué par le proxy)
#   -NoBundle           exécutable seul (target/release/tempo.exe), sans installeur
#   -SkipTools          ne pas télécharger les outils de conversion
#   -Test               lancer `cargo test` avant le build

param(
    [string[]]$Bundles = @("msi"),
    [switch]$NoBundle,
    [switch]$SkipTools,
    [switch]$Test
)

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot

function Step($message) { Write-Host "`n==> $message" -ForegroundColor Cyan }

# lance une commande native et s'arrête si elle échoue
function Run($exe, [string[]]$arguments, $cwd = $root) {
    Push-Location $cwd
    try {
        & $exe @arguments
        if ($LASTEXITCODE -ne 0) { throw "Échec de « $exe $($arguments -join ' ') » (code $LASTEXITCODE)" }
    } finally {
        Pop-Location
    }
}

$started = Get-Date

Step "Vérification des prérequis"
foreach ($tool in "cargo", "npm") {
    if (-not (Get-Command $tool -ErrorAction SilentlyContinue)) { throw "$tool est introuvable dans le PATH" }
}
& cargo tauri --version *> $null
if ($LASTEXITCODE -ne 0) { throw "CLI Tauri absente : cargo install tauri-cli --version `"^2`" --locked" }

$frontend = Join-Path $root "frontend"
if (-not (Test-Path (Join-Path $frontend "node_modules"))) {
    Step "Installation des dépendances de l'interface"
    Run "npm" @("ci") $frontend
}

if (-not $SkipTools) {
    Step "Outils de conversion (pandoc, pdfium, ONNX Runtime, modèle d'embeddings)"
    Run "powershell" @("-NoProfile", "-ExecutionPolicy", "Bypass", "-File", (Join-Path $PSScriptRoot "telecharger-outils.ps1"))
}

if ($Test) {
    Step "Tests"
    Run "cargo" @("test")
}

$tauri = Join-Path $root "src-tauri"
if ($NoBundle) {
    Step "Build release (exécutable seul)"
    Run "cargo" @("tauri", "build", "--no-bundle") $tauri
} else {
    $targets = ($Bundles | ForEach-Object { $_ -split "," }) -join ","
    Step "Build release (installeurs : $targets)"
    Run "cargo" @("tauri", "build", "--bundles", $targets) $tauri
}

Step "Terminé en $([int]((Get-Date) - $started).TotalMinutes) min"
$release = Join-Path $root "target\release"
Get-Item (Join-Path $release "tempo.exe") -ErrorAction SilentlyContinue |
    ForEach-Object { Write-Host "  $($_.FullName)" }
Get-ChildItem (Join-Path $release "bundle") -Recurse -Include "*.msi", "*.exe" -ErrorAction SilentlyContinue |
    ForEach-Object { Write-Host "  $($_.FullName)" }
