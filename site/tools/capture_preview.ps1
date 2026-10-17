# Capture the actual release app with a harmless sample document.
# Run from the repository root after: cargo build --release -p quintess-cli
param([string]$Python = 'python')
$ErrorActionPreference = 'Stop'
$repository = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
$program = Join-Path $repository 'target\release\quintess.exe'
$sample = Join-Path $repository 'scratch\workshop-notes.pdf'
$picture = Join-Path $repository 'site\public\assets\img\app-edit.png'
New-Item -ItemType Directory -Force (Split-Path $sample) | Out-Null
$creation = Start-Process -FilePath $program -ArgumentList @('create', 'markdown', ('"' + (Join-Path $PSScriptRoot 'preview-notes.md') + '"'), '--output', ('"' + $sample + '"')) -WindowStyle Hidden -PassThru -Wait
if ($creation.ExitCode -ne 0) { throw 'Could not create the preview document.' }
$previousScreenshot = $env:QUINTESS_SCREENSHOT
$previousSetup = $env:QUINTESS_SETUP
try {
    $env:QUINTESS_SCREENSHOT = $picture
    $env:QUINTESS_SETUP = 'studio,dark,zoom=1.1,edit-paragraph=4,wait=2'
    $capture = Start-Process -FilePath $program -ArgumentList ('"' + $sample + '"') -WindowStyle Hidden -PassThru
    $deadline = (Get-Date).AddMinutes(3)
    while (-not $capture.WaitForExit(10000)) {
        if ((Get-Date) -gt $deadline) { $capture.Kill(); throw 'Preview capture timed out.' }
    }
    if (-not (Test-Path $picture)) { throw 'The app did not save a screenshot.' }
    & $Python -c 'import sys; from PIL import Image; Image.open(sys.argv[1]).save(sys.argv[2], "WEBP", quality=90, method=6)' $picture ([IO.Path]::ChangeExtension($picture, '.webp'))
    if ($LASTEXITCODE -ne 0) { throw 'Could not create the WebP preview. Python with Pillow is required.' }
    Write-Output "Saved $picture"
} finally {
    $env:QUINTESS_SCREENSHOT = $previousScreenshot
    $env:QUINTESS_SETUP = $previousSetup
}
