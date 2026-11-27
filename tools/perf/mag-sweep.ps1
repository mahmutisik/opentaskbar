param([int]$FromX = 760, [int]$ToX = 1180, [int]$Y = 1055, [int]$Seconds = 6, [string]$Which = 'rust')
# Sweeps the pointer back and forth over the dock apps for $Seconds and reports the dock process CPU (% of one core
# and % of the machine) and private memory. $Which = 'rust' runs the Rust dock (C# stopped), 'cs' measures the C# dock.
$ErrorActionPreference = 'Continue'
$here = Split-Path -Parent $MyInvocation.MyCommand.Path  # shell-state.ps1 is expected next to this script
Add-Type 'using System.Runtime.InteropServices; public static class MagPerf { [DllImport("user32.dll")] public static extern bool SetProcessDPIAware(); [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y); }'
[MagPerf]::SetProcessDPIAware() | Out-Null
$cs = Join-Path $env:LOCALAPPDATA 'Programs\OpenTaskbar\OpenTaskbar.exe'
$exe = Join-Path $PSScriptRoot '..\..\target\release\OpenTaskBar.exe'
$settings = Join-Path $env:LOCALAPPDATA 'OpenTaskbar\settings.json'
$cores = [Environment]::ProcessorCount
function Measure-Sweep($proc) {
  $proc.Refresh(); $cpu0 = $proc.TotalProcessorTime.TotalMilliseconds
  $sw = [Diagnostics.Stopwatch]::StartNew(); $dir = 1; $x = $FromX
  while ($sw.ElapsedMilliseconds -lt $Seconds * 1000) {
    $x += 6 * $dir
    if ($x -ge $ToX -or $x -le $FromX) { $dir = -$dir }
    [MagPerf]::SetCursorPos($x, $Y) | Out-Null
    Start-Sleep -Milliseconds 8
  }
  $proc.Refresh(); $cpu = $proc.TotalProcessorTime.TotalMilliseconds - $cpu0
  $ms = $sw.ElapsedMilliseconds
  [MagPerf]::SetCursorPos(960, 500) | Out-Null
  Start-Sleep -Seconds 1
  $proc.Refresh(); $idle0 = $proc.TotalProcessorTime.TotalMilliseconds; Start-Sleep -Seconds 3; $proc.Refresh()
  $idle = $proc.TotalProcessorTime.TotalMilliseconds - $idle0
  "{0}: sweep CPU {1:N1}% of one core ({2:N1}% machine), idle after {3:N2}% of one core, private {4:N1} MB, threads {5}" -f $Which, (100 * $cpu / $ms), (100 * $cpu / $ms / $cores), (100 * $idle / 3000), ($proc.PrivateMemorySize64 / 1MB), $proc.Threads.Count
}
try {
  if ($Which -eq 'cs') {
    $p = Get-Process OpenTaskbar | Select-Object -First 1
    [MagPerf]::SetCursorPos(960, 1075) | Out-Null   # reveal the auto-hidden dock
    Start-Sleep -Milliseconds 800
    Measure-Sweep $p
  } else {
    Get-Process OpenTaskbar -ErrorAction SilentlyContinue | Stop-Process -Force
    Start-Sleep -Seconds 3
    [MagPerf]::SetCursorPos(960, 500) | Out-Null
    $p = Start-Process -FilePath $exe -ArgumentList @('--dock=30', "--use-settings=$settings") -PassThru
    Start-Sleep -Seconds 4
    Measure-Sweep $p
    $p.Kill(); Start-Sleep -Seconds 2
    & (Join-Path $here 'shell-state.ps1') 'after-kill'
  }
}
finally {
  if (-not (Get-Process OpenTaskbar -ErrorAction SilentlyContinue)) { Start-Process $cs | Out-Null; Start-Sleep -Seconds 4 }
  & (Join-Path $here 'shell-state.ps1') 'final'
}
