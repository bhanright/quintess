# Opens a PDF in a program several times and measures, each time:
#   - how long until the window appears,
#   - how long until the program is ready to use: the window is up and the
#     processes' combined CPU time then grows by less than 0.05 s per
#     quarter second for 2 seconds (the moment that quiet period began),
#   - the memory then: the sum of the working sets of every process in the
#     family (shared pages are counted once per process, as Task Manager's
#     Details tab counts them), and the peak private bytes.
# "Ready" does not mean every page has been rendered; it means the program
# has stopped working on its own after opening the file.
#
#   powershell -File tools/benchmark/benchmark.ps1 -Program <exe> -File <pdf> -ProcessNames name1,name2 [-Runs 5]
#
# The program is closed (all processes in the family) between runs. The
# first run reads the file from disk; later runs find it in the system's
# file cache, so the median of runs 2 and later is the warm figure.
param(
    [Parameter(Mandatory)] [string]$Program,
    [Parameter(Mandatory)] [string]$File,
    # Process names to total, without .exe (for Acrobat: Acrobat, AcroCEF, acrocef, RdrCEF).
    [Parameter(Mandatory)] [string[]]$ProcessNames,
    [int]$Runs = 5
)

function Get-Family([string[]]$names) {
    Get-Process -ErrorAction SilentlyContinue | Where-Object { $names -contains $_.ProcessName }
}

if (Get-Family $ProcessNames) { throw "Close $($ProcessNames -join ', ') before measuring." }

$results = @()
for ($run = 1; $run -le $Runs; $run++) {
    $start = Get-Date
    $null = Start-Process -FilePath $Program -ArgumentList "`"$File`"" -PassThru
    $windowAt = $null
    $quietSince = $null
    $lastCpu = 0
    $peakPrivate = 0
    while ((Get-Date) - $start -lt [TimeSpan]::FromSeconds(90)) {
        Start-Sleep -Milliseconds 250
        $family = Get-Family $ProcessNames
        if (-not $family) { continue }
        if (-not $windowAt -and ($family | Where-Object { $_.MainWindowHandle -ne 0 })) { $windowAt = (Get-Date) - $start }
        $cpu = ($family | Measure-Object CPU -Sum).Sum
        $private = ($family | Measure-Object PrivateMemorySize64 -Sum).Sum
        if ($private -gt $peakPrivate) { $peakPrivate = $private }
        if ($windowAt) {
            if (($cpu - $lastCpu) -lt 0.05) {
                if (-not $quietSince) { $quietSince = Get-Date }
                elseif ((Get-Date) - $quietSince -gt [TimeSpan]::FromSeconds(2)) { break }
            } else { $quietSince = $null }
        }
        $lastCpu = $cpu
    }
    $ready = if ($quietSince) { ($quietSince - $start).TotalSeconds } else { 90 }
    $family = Get-Family $ProcessNames
    $results += [pscustomobject]@{
        Run = $run
        WindowSeconds = [math]::Round($windowAt.TotalSeconds, 2)
        ReadySeconds = [math]::Round($ready, 2)
        WorkingSetMB = [math]::Round((($family | Measure-Object WorkingSet64 -Sum).Sum) / 1MB, 0)
        PeakPrivateMB = [math]::Round($peakPrivate / 1MB, 0)
        Processes = ($family | Measure-Object).Count
    }
    $family | Stop-Process -Force -ErrorAction SilentlyContinue
    Start-Sleep -Seconds 3
}
$results | Format-Table -AutoSize | Out-String -Width 200

$warm = $results | Select-Object -Skip 1
function Median($values) { $sorted = $values | Sort-Object; $sorted[[math]::Floor(($sorted.Count - 1) / 2)] }
"Warm median (runs 2-$Runs): ready $(Median ($warm.ReadySeconds)) s, working set $(Median ($warm.WorkingSetMB)) MB, peak private $(Median ($warm.PeakPrivateMB)) MB"
