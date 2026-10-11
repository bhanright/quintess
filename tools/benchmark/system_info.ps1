# Prints the details a benchmark result needs beside it: Windows version,
# processor and its current speed, memory, graphics, disk, and the versions
# of the programs measured.
#
#   powershell -File tools/benchmark/system_info.ps1 -Programs <exe>,<exe>
param([string[]]$Programs = @())

$os = Get-CimInstance Win32_OperatingSystem
$cpu = Get-CimInstance Win32_Processor | Select-Object -First 1
$speed = (Get-Counter '\Processor Information(_Total)\% Processor Performance' -ErrorAction SilentlyContinue).CounterSamples.CookedValue
"Windows: $($os.Caption) $($os.Version) (build $($os.BuildNumber))"
"Processor: $($cpu.Name.Trim()), $($cpu.NumberOfCores) cores / $($cpu.NumberOfLogicalProcessors) threads, base $($cpu.MaxClockSpeed) MHz"
if ($speed) { "Processor running at about $([math]::Round($cpu.MaxClockSpeed * $speed / 100)) MHz now" }
"Memory: $([math]::Round($os.TotalVisibleMemorySize / 1MB, 1)) GB"
Get-CimInstance Win32_VideoController | ForEach-Object { "Graphics: $($_.Name) (driver $($_.DriverVersion))" }
Get-PhysicalDisk | ForEach-Object { "Disk: $($_.FriendlyName), $($_.MediaType), $([math]::Round($_.Size / 1GB)) GB" }
foreach ($program in $Programs) {
    $item = Get-Item $program
    "Program: $($item.Name) $($item.VersionInfo.ProductVersion) (file $($item.VersionInfo.FileVersion))"
}
