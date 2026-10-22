# Télécharge les outils de conversion de la bibliothèque dans src-tauri/resources/ :
# pandoc.exe (docx, odt, rtf, html, epub), pdfium.dll (PDF), ONNX Runtime et le modèle
# d'embeddings multilingual-e5-small quantifié (recherche par le sens). Ils sont livrés avec
# l'installeur (bundle.resources) et servent aussi en développement (repli de tempo-core).
# Usage : powershell -ExecutionPolicy Bypass -File scripts/telecharger-outils.ps1 [-Force]

param([switch]$Force)

$ErrorActionPreference = "Stop"
$ProgressPreference = "SilentlyContinue"

# versions figées : pdfium doit correspondre à la fonctionnalité pdfium_<version> de pdfium-render
$PandocVersion = "3.12.1"
$PdfiumVersion = "7881"
# ONNX Runtime doit correspondre à la version visée par la crate ort (2.0.0-rc.13, API 27 : 1.27 ou plus récent)
$OrtVersion = "1.31.0"
$Model = "Xenova/multilingual-e5-small"

$root = Split-Path -Parent $PSScriptRoot
$resources = Join-Path $root "src-tauri\resources"
$work = Join-Path ([System.IO.Path]::GetTempPath()) ("tempo-outils-" + [guid]::NewGuid().ToString("N"))
New-Item -ItemType Directory -Force $resources, $work | Out-Null

try {
    $pandoc = Join-Path $resources "pandoc.exe"
    if ($Force -or -not (Test-Path $pandoc)) {
        Write-Host "pandoc $PandocVersion…"
        $zip = Join-Path $work "pandoc.zip"
        Invoke-WebRequest "https://github.com/jgm/pandoc/releases/download/$PandocVersion/pandoc-$PandocVersion-windows-x86_64.zip" -OutFile $zip
        Expand-Archive $zip -DestinationPath (Join-Path $work "pandoc")
        Copy-Item (Get-ChildItem (Join-Path $work "pandoc") -Recurse -Filter "pandoc.exe" | Select-Object -First 1).FullName $pandoc
    }

    $pdfium = Join-Path $resources "pdfium.dll"
    if ($Force -or -not (Test-Path $pdfium)) {
        Write-Host "pdfium $PdfiumVersion…"
        $tgz = Join-Path $work "pdfium.tgz"
        Invoke-WebRequest "https://github.com/bblanchon/pdfium-binaries/releases/download/chromium%2F$PdfiumVersion/pdfium-win-x64.tgz" -OutFile $tgz
        $extract = Join-Path $work "pdfium"
        New-Item -ItemType Directory -Force $extract | Out-Null
        tar -xzf $tgz -C $extract
        Copy-Item (Join-Path $extract "bin\pdfium.dll") $pdfium
    }
    $ort = Join-Path $resources "onnxruntime.dll"
    if ($Force -or -not (Test-Path $ort)) {
        Write-Host "ONNX Runtime $OrtVersion..."
        $zip = Join-Path $work "ort.zip"
        Invoke-WebRequest "https://github.com/microsoft/onnxruntime/releases/download/v$OrtVersion/onnxruntime-win-x64-$OrtVersion.zip" -OutFile $zip
        Expand-Archive $zip -DestinationPath (Join-Path $work "ort")
        Copy-Item (Get-ChildItem (Join-Path $work "ort") -Recurse -Filter "onnxruntime.dll" | Select-Object -First 1).FullName $ort
    }

    $modelDir = Join-Path $resources "models/multilingual-e5-small"
    New-Item -ItemType Directory -Force $modelDir | Out-Null
    foreach ($file in @("onnx/model_int8.onnx", "tokenizer.json")) {
        $target = Join-Path $modelDir (Split-Path $file -Leaf)
        if ($Force -or -not (Test-Path $target)) {
            Write-Host "$Model : $file..."
            Invoke-WebRequest "https://huggingface.co/$Model/resolve/main/$file" -OutFile $target
        }
    }
    Write-Host "Outils prêts dans $resources"
}
finally {
    Remove-Item -Recurse -Force $work -ErrorAction SilentlyContinue
}
