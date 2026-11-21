param(
    [string]$Exe = (Join-Path $PSScriptRoot '..\target\release\windows-monitor.exe'),
    [ValidateRange(5, 300)][int]$MeasureSeconds = 30
)

$ErrorActionPreference = 'Stop'
$Exe = (Resolve-Path -LiteralPath $Exe).Path

if (-not ('MonitorSmoke.Native' -as [type])) {
    Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;

namespace MonitorSmoke {
    public static class Native {
        [StructLayout(LayoutKind.Sequential)]
        public struct Rect { public int Left, Top, Right, Bottom; }

        [StructLayout(LayoutKind.Sequential)]
        public struct ThreadInfo {
            public int Size, Flags;
            public IntPtr Active, Focus, Capture, MenuOwner, MoveSize, Caret;
            public Rect CaretRect;
        }

        [DllImport("user32.dll", CharSet = CharSet.Unicode)]
        public static extern IntPtr FindWindow(string className, IntPtr title);
        [DllImport("user32.dll")]
        public static extern uint GetWindowThreadProcessId(IntPtr hwnd, out uint pid);
        [DllImport("user32.dll")]
        public static extern bool GetGUIThreadInfo(uint thread, ref ThreadInfo info);
        [DllImport("user32.dll")]
        public static extern bool GetWindowRect(IntPtr hwnd, out Rect rect);
        [DllImport("user32.dll")]
        public static extern bool IsWindowVisible(IntPtr hwnd);
        [DllImport("user32.dll")]
        public static extern uint GetDpiForWindow(IntPtr hwnd);
        [DllImport("user32.dll")]
        public static extern IntPtr GetWindowDpiAwarenessContext(IntPtr hwnd);
        [DllImport("user32.dll")]
        public static extern IntPtr SetThreadDpiAwarenessContext(IntPtr context);
        [DllImport("user32.dll")]
        public static extern bool AreDpiAwarenessContextsEqual(IntPtr first, IntPtr second);
        [DllImport("user32.dll", EntryPoint = "GetWindowLongPtrW")]
        public static extern IntPtr GetWindowLongPtr(IntPtr hwnd, int index);
        [DllImport("user32.dll")]
        public static extern bool PostMessageW(IntPtr hwnd, uint message, UIntPtr wp, IntPtr lp);
    }
}
'@
}

function Assert-True($Value, [string]$Message) {
    if (-not $Value) { throw $Message }
}

function Wait-For([scriptblock]$Condition, [string]$Message) {
    for ($attempt = 0; $attempt -lt 50; $attempt++) {
        if (& $Condition) { return }
        Start-Sleep -Milliseconds 100
    }
    throw $Message
}

function Send-Panel([uint32]$Message, [uint32]$Command = 0, [intptr]$Data = [intptr]::Zero) {
    Assert-True ([MonitorSmoke.Native]::PostMessageW($window, $Message, [uintptr]$Command, $Data)) 'Posting to the panel failed.'
}

function Start-Monitor {
    $info = [Diagnostics.ProcessStartInfo]::new($Exe)
    $info.UseShellExecute = $false
    $info.EnvironmentVariables['LOCALAPPDATA'] = $testData
    [Diagnostics.Process]::Start($info)
}

function Get-PanelWindow {
    [MonitorSmoke.Native]::FindWindow('WindowsMonitor.Panel', [intptr]::Zero)
}

function Is-InMenu {
    $info = [MonitorSmoke.Native+ThreadInfo]::new()
    $info.Size = [Runtime.InteropServices.Marshal]::SizeOf($info)
    [MonitorSmoke.Native]::GetGUIThreadInfo($guiThread, [ref]$info) -and (($info.Flags -band 0x4) -ne 0)
}

Assert-True ((Get-PanelWindow) -eq [intptr]::Zero) 'Close the existing Windows Monitor instance before running the desktop smoke test.'
$testData = Join-Path ([IO.Path]::GetTempPath()) ('windows-monitor-smoke-' + [guid]::NewGuid())
$stateFile = Join-Path $testData 'WindowsMonitor\state.ini'
[void][IO.Directory]::CreateDirectory((Split-Path -Parent $stateFile))
[IO.File]::WriteAllLines($stateFile, @('traffic=16360000000', 'theme=light', 'topmost=true'))
$app = $second = $null
try {
    $app = Start-Monitor
    Wait-For { (Get-PanelWindow) -ne [intptr]::Zero } 'Panel did not open.'
    $window = Get-PanelWindow
    [uint32]$owner = 0
    $guiThread = [MonitorSmoke.Native]::GetWindowThreadProcessId($window, [ref]$owner)
    Assert-True ($owner -eq $app.Id) 'The launched instance must own the window.'
    Wait-For { [MonitorSmoke.Native]::IsWindowVisible($window) } 'Panel was not visible.'

    $rect = [MonitorSmoke.Native+Rect]::new()
    $oldDpiContext = [MonitorSmoke.Native]::SetThreadDpiAwarenessContext([intptr](-4))
    try {
        Assert-True ([MonitorSmoke.Native]::GetWindowRect($window, [ref]$rect)) 'Cannot get panel bounds.'
    } finally {
        if ($oldDpiContext -ne [intptr]::Zero) {
            [void][MonitorSmoke.Native]::SetThreadDpiAwarenessContext($oldDpiContext)
        }
    }
    $dpi = [MonitorSmoke.Native]::GetDpiForWindow($window)
    $width = $rect.Right - $rect.Left
    $height = $rect.Bottom - $rect.Top
    Assert-True ($width -eq [math]::Floor((420 * $dpi + 48) / 96)) 'Incorrect DPI-scaled panel width.'
    Assert-True ($height -eq [math]::Floor((68 * $dpi + 48) / 96)) 'Incorrect DPI-scaled panel height.'
    $dpiContext = [MonitorSmoke.Native]::GetWindowDpiAwarenessContext($window)
    Assert-True ([MonitorSmoke.Native]::AreDpiAwarenessContextsEqual($dpiContext, [intptr](-4))) 'Per-monitor V2 DPI awareness must be enabled.'

    $coordinates = (($rect.Top + 10) -band 0xffff) -shl 16
    $coordinates = $coordinates -bor (($rect.Left + 10) -band 0xffff)
    Send-Panel 0x7b 0 ([intptr]$coordinates)
    Wait-For { Is-InMenu } 'The native context menu did not open.'
    Send-Panel 0x1f
    Wait-For { -not (Is-InMenu) } 'The native context menu did not close.'

    Send-Panel 0x8002 0 ([intptr]0x10400)
    Wait-For { -not [MonitorSmoke.Native]::IsWindowVisible($window) } 'The tray callback did not hide the panel.'
    $second = Start-Monitor
    Assert-True ($second.WaitForExit(5000) -and $second.ExitCode -eq 0) 'The second process did not yield to the first.'
    Wait-For { [MonitorSmoke.Native]::IsWindowVisible($window) } 'A second launch did not reveal the original panel.'
    Assert-True ((Get-PanelWindow) -eq $window) 'A second launch replaced the original window.'

    Send-Panel 0x111 105
    Wait-For { (Get-Content $stateFile) -contains 'theme=dark' } 'Dark theme was not saved.'
    Send-Panel 0x111 104
    Wait-For { (Get-Content $stateFile) -contains 'theme=light' } 'Light theme was not saved.'
    Send-Panel 0x111 102
    Wait-For { ([MonitorSmoke.Native]::GetWindowLongPtr($window, -20).ToInt64() -band 8) -eq 0 } 'Topmost did not turn off.'
    Send-Panel 0x111 102
    Wait-For { ([MonitorSmoke.Native]::GetWindowLongPtr($window, -20).ToInt64() -band 8) -ne 0 } 'Topmost did not turn back on.'
    Send-Panel 0x111 106
    Wait-For { (Get-Content $stateFile) -contains 'traffic=0' } 'Reset did not clear the persisted counter.'

    $app.Refresh()
    $cpuStart = $app.TotalProcessorTime.TotalSeconds
    $timer = [Diagnostics.Stopwatch]::StartNew()
    $peakPrivateWorkingSet = 0
    $peakWorkingSet = 0
    while ($true) {
        $app.Refresh()
        Assert-True (-not $app.HasExited) 'The process exited during the resource measurement.'
        $counters = Get-CimInstance Win32_PerfFormattedData_PerfProc_Process -Filter ('IDProcess={0}' -f $app.Id)
        Assert-True ($null -ne $counters) 'Cannot read private working set from Windows performance counters.'
        $peakPrivateWorkingSet = [math]::Max($peakPrivateWorkingSet, [double]$counters.WorkingSetPrivate)
        $peakWorkingSet = [math]::Max($peakWorkingSet, [double]$app.WorkingSet64)
        $remaining = $MeasureSeconds - $timer.Elapsed.TotalSeconds
        if ($remaining -le 0) { break }
        Start-Sleep -Milliseconds ([int]([math]::Min(5000, 1000 * $remaining)))
    }
    $cpu = 100 * ($app.TotalProcessorTime.TotalSeconds - $cpuStart) / $timer.Elapsed.TotalSeconds / [Environment]::ProcessorCount

    Send-Panel 0x111 107
    Assert-True ($app.WaitForExit(5000) -and $app.ExitCode -eq 0) 'Exit did not shut down cleanly.'
    Assert-True ((Get-PanelWindow) -eq [intptr]::Zero) 'The window remained after exit.'
    Assert-True ((Get-Content $stateFile) -contains 'theme=light') 'Settings were not preserved at exit.'

    [pscustomobject]@{
        Passed                         = $true
        Dpi                            = $dpi
        PanelPixels                    = ('{0}x{1}' -f $width, $height)
        LogicalProcessors              = [Environment]::ProcessorCount
        ExecutableBytes                = (Get-Item $Exe).Length
        SamplingSeconds                = [math]::Round($timer.Elapsed.TotalSeconds, 2)
        AverageCpuPercent              = [math]::Round($cpu, 4)
        PeakPrivateWorkingSetMiB       = [math]::Round(($peakPrivateWorkingSet / 1MB), 2)
        PeakWorkingSetMiB              = [math]::Round(($peakWorkingSet / 1MB), 2)
    } | ConvertTo-Json
} finally {
    foreach ($process in @($app, $second)) {
        if ($null -ne $process) {
            if (-not $process.HasExited) {
                $process.Kill()
                [void]$process.WaitForExit(5000)
            }
            $process.Dispose()
        }
    }
    Remove-Item -LiteralPath $testData -Recurse -Force
}
