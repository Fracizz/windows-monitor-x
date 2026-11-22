param(
    [string]$Exe = (Join-Path $PSScriptRoot '..\target\release\windows-monitor.exe'),
    [ValidateRange(5, 300)][int]$MeasureSeconds = 30,
    [switch]$RequireTaskbar,
    [switch]$ExpectDockHidden
)

$ErrorActionPreference = 'Stop'
$Exe = (Resolve-Path -LiteralPath $Exe).Path
if ($RequireTaskbar -and $ExpectDockHidden) {
    throw 'Choose either visible or hidden taskbar validation.'
}

if (-not ('MonitorSmoke.Native' -as [type])) {
    Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
using System.Text;

namespace MonitorSmoke {
    public static class Native {
        [StructLayout(LayoutKind.Sequential)]
        public struct Rect { public int Left, Top, Right, Bottom; }

        [StructLayout(LayoutKind.Sequential)]
        public struct Point { public int X, Y; }

        [StructLayout(LayoutKind.Sequential)]
        public struct ThreadInfo {
            public int Size, Flags;
            public IntPtr Active, Focus, Capture, MenuOwner, MoveSize, Caret;
            public Rect CaretRect;
        }

        [DllImport("user32.dll", CharSet = CharSet.Unicode)]
        public static extern IntPtr FindWindow(string className, IntPtr title);
        [DllImport("user32.dll", CharSet = CharSet.Unicode)]
        public static extern int GetWindowText(IntPtr hwnd, StringBuilder text, int capacity);
        [DllImport("user32.dll")]
        public static extern uint GetWindowThreadProcessId(IntPtr hwnd, out uint pid);
        [DllImport("user32.dll")]
        public static extern bool GetGUIThreadInfo(uint thread, ref ThreadInfo info);
        [DllImport("user32.dll")]
        public static extern IntPtr GetForegroundWindow();
        [DllImport("user32.dll")]
        public static extern bool GetWindowRect(IntPtr hwnd, out Rect rect);
        [DllImport("user32.dll")]
        public static extern IntPtr WindowFromPoint(Point point);
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
        public static extern IntPtr GetDlgItem(IntPtr hwnd, int id);
        [DllImport("user32.dll")]
        public static extern IntPtr SendMessageW(IntPtr hwnd, uint message, UIntPtr wp, IntPtr lp);
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

function Start-Monitor([switch]$Background) {
    $info = [Diagnostics.ProcessStartInfo]::new($Exe)
    $info.UseShellExecute = $false
    $info.EnvironmentVariables['LOCALAPPDATA'] = $testData
    if ($Background) { $info.Arguments = '--background' }
    [Diagnostics.Process]::Start($info)
}

function Get-PanelWindow {
    [MonitorSmoke.Native]::FindWindow('WindowsMonitor.Panel', [intptr]::Zero)
}

function Get-ManagerWindow {
    [MonitorSmoke.Native]::FindWindow('WindowsMonitor.Manager', [intptr]::Zero)
}

function Test-ManagerVisible {
    $handle = Get-ManagerWindow
    $handle -ne [intptr]::Zero -and [MonitorSmoke.Native]::IsWindowVisible($handle)
}

function Send-ManagerControl([uint32]$Id, [int]$Selection = -1) {
    $control = [MonitorSmoke.Native]::GetDlgItem($manager, $Id)
    Assert-True ($control -ne [intptr]::Zero) "Manager control $Id is missing."
    $command = $Id
    if ($Selection -ge 0) {
        $selected = [MonitorSmoke.Native]::SendMessageW($control, 0x14e, [uintptr]$Selection, [intptr]::Zero)
        Assert-True ($selected.ToInt64() -eq $Selection) "Could not select option $Selection for control $Id."
        $command = $command -bor 0x10000
    }
    Assert-True ([MonitorSmoke.Native]::PostMessageW($manager, 0x111, [uintptr]$command, $control)) "Could not use manager control $Id."
}

function Get-WindowTitle([intptr]$Handle) {
    $text = [Text.StringBuilder]::new(128)
    [void][MonitorSmoke.Native]::GetWindowText($Handle, $text, $text.Capacity)
    $text.ToString()
}

function Test-Setting([string]$Entry) {
    $reader = $file = $null
    try {
        $file = [IO.File]::Open($stateFile, 'Open', 'Read', ([IO.FileShare]::ReadWrite -bor [IO.FileShare]::Delete))
        $reader = [IO.StreamReader]::new($file)
        return ($reader.ReadToEnd() -split '\r?\n') -contains $Entry
    } catch [IO.IOException] {
        return $false
    } finally {
        if ($null -ne $reader) {
            $reader.Dispose()
        } elseif ($null -ne $file) {
            $file.Dispose()
        }
    }
}

function Get-Bounds([intptr]$Handle) {
    $rect = [MonitorSmoke.Native+Rect]::new()
    $oldDpiContext = [MonitorSmoke.Native]::SetThreadDpiAwarenessContext([intptr](-4))
    try {
        Assert-True ([MonitorSmoke.Native]::GetWindowRect($Handle, [ref]$rect)) 'Cannot get panel bounds.'
    } finally {
        if ($oldDpiContext -ne [intptr]::Zero) {
            [void][MonitorSmoke.Native]::SetThreadDpiAwarenessContext($oldDpiContext)
        }
    }
    $rect
}

function Test-PanelSize([int]$ScalableWidth, [int]$GapPixels, [int]$Height) {
    $panel = Get-Bounds $window
    $dpi = [MonitorSmoke.Native]::GetDpiForWindow($window)
    return (
        ($panel.Right - $panel.Left) -eq ([math]::Ceiling($ScalableWidth * $dpi / 96) + $GapPixels) -and
        ($panel.Bottom - $panel.Top) -eq [math]::Floor(($Height * $dpi + 48) / 96)
    )
}

function Test-TaskbarPlacement {
    $panel = Get-Bounds $window
    $taskbar = [MonitorSmoke.Native]::FindWindow('Shell_TrayWnd', [intptr]::Zero)
    if ($taskbar -eq [intptr]::Zero) { return $false }
    $bar = Get-Bounds $taskbar
    $taskbarDpi = [MonitorSmoke.Native]::GetDpiForWindow($taskbar)
    return (
        [MonitorSmoke.Native]::IsWindowVisible($window) -and
        ($panel.Right - $panel.Left) -eq ([math]::Ceiling(246 * $taskbarDpi / 96) + 12) -and
        ($panel.Bottom - $panel.Top) -eq [math]::Floor((36 * $taskbarDpi + 48) / 96) -and
        $panel.Left -ge $bar.Left -and $panel.Right -le $bar.Right -and
        $panel.Top -ge $bar.Top -and $panel.Bottom -le $bar.Bottom
    )
}

function Test-PanelOnTopAtCenter {
    $panel = Get-Bounds $window
    $point = [MonitorSmoke.Native+Point]::new()
    $point.X = [int](($panel.Right + $panel.Left) / 2)
    $point.Y = [int](($panel.Bottom + $panel.Top) / 2)
    $old = [MonitorSmoke.Native]::SetThreadDpiAwarenessContext([intptr](-4))
    try {
        return [MonitorSmoke.Native]::WindowFromPoint($point) -eq $window
    } finally {
        if ($old -ne [intptr]::Zero) { [void][MonitorSmoke.Native]::SetThreadDpiAwarenessContext($old) }
    }
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
[IO.File]::WriteAllLines($stateFile, @('traffic=16360000000', 'theme=light', 'language=en-US', 'topmost=true'))
$app = $second = $null
try {
    $app = Start-Monitor
    Wait-For { (Get-PanelWindow) -ne [intptr]::Zero } 'Panel did not open.'
    $window = Get-PanelWindow
    [uint32]$owner = 0
    $guiThread = [MonitorSmoke.Native]::GetWindowThreadProcessId($window, [ref]$owner)
    Assert-True ($owner -eq $app.Id) 'The launched instance must own the window.'
    Wait-For { [MonitorSmoke.Native]::IsWindowVisible($window) } 'Panel was not visible.'
    Wait-For { Test-PanelSize 320 24 68 } 'Incorrect DPI-scaled English panel dimensions after startup.'
    Wait-For { Test-ManagerVisible } 'Starting manually did not open the manager.'
    $manager = Get-ManagerWindow
    Assert-True ((Get-WindowTitle $manager).StartsWith('Manager')) 'Manager language was not restored.'
    $liveList = [MonitorSmoke.Native]::GetDlgItem($manager, 205)
    Assert-True ($liveList -ne [intptr]::Zero) 'Live readings were missing from the manager.'
    $rows = [MonitorSmoke.Native]::SendMessageW($liveList, 0x1004, [uintptr]0, [intptr]::Zero)
    Assert-True ($rows.ToInt64() -eq 16) 'The manager did not show all live metrics.'
    Assert-True ([MonitorSmoke.Native]::PostMessageW($manager, 0x10, [uintptr]0, [intptr]::Zero)) 'Closing the manager failed.'
    Wait-For { (Get-ManagerWindow) -eq [intptr]::Zero } 'The manager did not close.'
    Assert-True ((Get-PanelWindow) -eq $window -and [MonitorSmoke.Native]::IsWindowVisible($window)) 'Closing the manager interrupted the panel.'

    $rect = Get-Bounds $window
    $dpi = [MonitorSmoke.Native]::GetDpiForWindow($window)
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
    $second = Start-Monitor -Background
    Assert-True ($second.WaitForExit(5000) -and $second.ExitCode -eq 0) 'The background launch did not yield to the first instance.'
    Assert-True ((Get-ManagerWindow) -eq [intptr]::Zero -and -not [MonitorSmoke.Native]::IsWindowVisible($window)) 'The background launch changed the visible windows.'
    $second.Dispose()
    $second = Start-Monitor
    Assert-True ($second.WaitForExit(5000) -and $second.ExitCode -eq 0) 'The second process did not yield to the first.'
    Wait-For { [MonitorSmoke.Native]::IsWindowVisible($window) } 'A second launch did not reveal the original panel.'
    Assert-True ((Get-PanelWindow) -eq $window) 'A second launch replaced the original window.'
    Wait-For { Test-ManagerVisible } 'A second launch did not reopen the manager.'
    $manager = Get-ManagerWindow
    Send-ManagerControl 101
    Wait-For { -not [MonitorSmoke.Native]::IsWindowVisible($window) } 'The manager did not hide the panel.'

    Send-ManagerControl 124
    Wait-For {
        $script:historyWindow = [MonitorSmoke.Native]::FindWindow('WindowsMonitor.History', [intptr]::Zero)
        $historyWindow -ne [intptr]::Zero -and [MonitorSmoke.Native]::IsWindowVisible($historyWindow)
    } 'History did not open from the manager.'
    Assert-True ([MonitorSmoke.Native]::PostMessageW($historyWindow, 0x10, [uintptr]0, [intptr]::Zero)) 'Closing History failed.'
    Wait-For { [MonitorSmoke.Native]::FindWindow('WindowsMonitor.History', [intptr]::Zero) -eq [intptr]::Zero } 'History did not close.'
    Send-ManagerControl 101
    Wait-For { [MonitorSmoke.Native]::IsWindowVisible($window) } 'The manager did not show the panel again.'

    Send-ManagerControl 202 2
    Wait-For { Test-Setting 'theme=dark' } 'Dark theme was not saved.'
    Send-Panel 0x111 118
    Wait-For { Test-Setting 'theme=transparent' } 'Transparent theme was not saved.'
    Send-Panel 0x7b 0 ([intptr]$coordinates)
    Wait-For { Is-InMenu } 'The transparent panel menu did not open.'
    $menuInfo = [MonitorSmoke.Native+ThreadInfo]::new()
    $menuInfo.Size = [Runtime.InteropServices.Marshal]::SizeOf($menuInfo)
    Assert-True ([MonitorSmoke.Native]::GetGUIThreadInfo($guiThread, [ref]$menuInfo)) 'Could not inspect the transparent panel menu.'
    Assert-True ($menuInfo.MenuOwner -ne [intptr]::Zero -and $menuInfo.MenuOwner -ne $window) 'The painted panel owns the transparent menu.'
    Assert-True ([MonitorSmoke.Native]::GetForegroundWindow() -ne $window) 'Opening the transparent menu activated the painted panel.'
    Assert-True ([MonitorSmoke.Native]::PostMessageW($menuInfo.MenuOwner, 0x1f, [uintptr]0, [intptr]::Zero)) 'Could not dismiss the transparent menu.'
    Wait-For { -not (Is-InMenu) } 'The transparent panel menu did not close.'
    Send-Panel 0x111 104
    Wait-For { Test-Setting 'theme=light' } 'Light theme was not saved.'
    Send-Panel 0x111 119
    Wait-For { Test-Setting 'label_bold=false' } 'Label bold must be enabled by default and can be turned off.'
    Send-Panel 0x111 102
    Wait-For { ([MonitorSmoke.Native]::GetWindowLongPtr($window, -20).ToInt64() -band 8) -eq 0 } 'Topmost did not turn off.'
    Send-Panel 0x111 102
    Wait-For { ([MonitorSmoke.Native]::GetWindowLongPtr($window, -20).ToInt64() -band 8) -ne 0 } 'Topmost did not turn back on.'
    Send-Panel 0x111 106
    Wait-For { Test-Setting 'traffic=0' } 'Reset did not clear the persisted counter.'

    foreach ($option in @(@(110, 2), @(111, 3), @(112, 5), @(109, 1))) {
        Send-Panel 0x111 $option[0]
        Wait-For { Test-Setting "refresh=$($option[1])" } 'Refresh interval was not saved.'
    }

    Send-ManagerControl 116
    $editorClass = 'WindowsMonitor.MetricsEditor'
    Wait-For {
        $handle = [MonitorSmoke.Native]::FindWindow($editorClass, [intptr]::Zero)
        $handle -ne [intptr]::Zero -and [MonitorSmoke.Native]::IsWindowVisible($handle)
    } 'The metric editor did not open.'
    $editor = [MonitorSmoke.Native]::FindWindow($editorClass, [intptr]::Zero)
    Send-ManagerControl 204 1
    Wait-For {
        (Test-Setting 'language=zh-CN') -and (Get-WindowTitle $editor) -cne 'Edit Metrics' -and
            -not (Get-WindowTitle $manager).StartsWith('Manager')
    } 'Chinese did not apply to the open manager and editor.'
    Wait-For { Test-PanelSize 333 24 68 } 'The panel did not resize for the Chinese labels.'
    Send-Panel 0x111 123
    Wait-For {
        (Test-Setting 'language=en-US') -and (Get-WindowTitle $editor) -ceq 'Edit Metrics' -and
            (Get-WindowTitle $manager).StartsWith('Manager')
    } 'English did not apply to the open manager and editor.'
    Wait-For { Test-PanelSize 320 24 68 } 'The panel did not resize for the English labels.'
    $density = [MonitorSmoke.Native]::GetDlgItem($editor, 15)
    Assert-True ($density -ne [intptr]::Zero) 'The density dropdown was missing.'
    $selected = [MonitorSmoke.Native]::SendMessageW($density, 0x14e, [uintptr]2, [intptr]::Zero)
    Assert-True ($selected.ToInt64() -eq 2) 'The tight density could not be selected.'
    Assert-True ([MonitorSmoke.Native]::PostMessageW($editor, 0x111, [uintptr]0x1000f, $density)) 'Changing density failed.'
    Assert-True ([MonitorSmoke.Native]::PostMessageW($editor, 0x111, [uintptr]1, [intptr]::Zero)) 'Applying density failed.'
    Wait-For { (Test-Setting 'density=tight') -and (Test-PanelSize 265 12 68) } 'Tight density was not saved and applied.'
    $rect = Get-Bounds $window
    $width = $rect.Right - $rect.Left
    $height = $rect.Bottom - $rect.Top

    $dockedPixels = $null
    if ($RequireTaskbar -or $ExpectDockHidden) {
        Send-ManagerControl 108
        Wait-For { Test-Setting 'taskbar=true' } 'Dock preference was not saved.'
    }
    Assert-True ([MonitorSmoke.Native]::PostMessageW($manager, 0x10, [uintptr]0, [intptr]::Zero)) 'Closing the manager after configuration failed.'
    Wait-For { (Get-ManagerWindow) -eq [intptr]::Zero } 'The manager was still open before measuring background monitoring.'
    if ($RequireTaskbar -or $ExpectDockHidden) {
        if ($RequireTaskbar) {
            Wait-For { Test-TaskbarPlacement } 'The panel did not enter an available taskbar slot.'
            Wait-For { Test-PanelOnTopAtCenter } 'The taskbar panel is not visible above Explorer.'
            $docked = Get-Bounds $window
            $dockedPixels = '{0}x{1}' -f ($docked.Right - $docked.Left), ($docked.Bottom - $docked.Top)
        } else {
            Wait-For { -not [MonitorSmoke.Native]::IsWindowVisible($window) } 'The panel did not hide with the taskbar covered.'
        }
    }

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

    Send-Panel 0x111 112
    Wait-For { Test-Setting 'refresh=5' } 'Refresh setting was not saved at exit.'
    Send-Panel 0x111 107
    Assert-True ($app.WaitForExit(5000) -and $app.ExitCode -eq 0) 'Exit did not shut down cleanly.'
    Assert-True ((Get-PanelWindow) -eq [intptr]::Zero) 'The window remained after exit.'
    Assert-True (Test-Setting 'theme=light') 'Settings were not preserved at exit.'

    $app.Dispose()
    $app = Start-Monitor -Background
    Wait-For { (Get-PanelWindow) -ne [intptr]::Zero } 'Panel did not reopen.'
    $window = Get-PanelWindow
    if ($RequireTaskbar -or $ExpectDockHidden) {
        if ($RequireTaskbar) {
            Wait-For { Test-TaskbarPlacement } 'Dock preference was not restored after restart.'
            Wait-For { Test-PanelOnTopAtCenter } 'Explorer covered the taskbar panel after restart.'
        } else {
            Wait-For { Test-Setting 'taskbar=true' } 'Dock preference was not preserved after restart.'
            Wait-For { -not [MonitorSmoke.Native]::IsWindowVisible($window) } 'The restarted panel should become hidden.'
        }
        Send-Panel 0x111 108
        Wait-For { Test-Setting 'taskbar=false' } 'Taskbar mode did not turn off.'
    }
    Wait-For { Test-PanelSize 265 12 68 } 'Tight floating dimensions were not restored after restart.'
    Assert-True ((Get-ManagerWindow) -eq [intptr]::Zero) 'A background startup opened the manager.'
    Send-Panel 0x111 125
    Wait-For { Test-ManagerVisible } 'The Open Manager command did not open the manager.'
    $manager = Get-ManagerWindow
    Send-Panel 0x111 116
    Wait-For {
        $script:editor = [MonitorSmoke.Native]::FindWindow($editorClass, [intptr]::Zero)
        $editor -ne [intptr]::Zero -and (Get-WindowTitle $editor) -ceq 'Edit Metrics'
    } 'The English preference was not restored after restart.'
    Assert-True ([MonitorSmoke.Native]::PostMessageW($editor, 0x111, [uintptr]2, [intptr]::Zero)) 'Closing the metrics editor failed.'
    Send-Panel 0x111 119
    Wait-For { Test-Setting 'label_bold=true' } 'The label weight preference was not restored after restart.'
    Send-ManagerControl 107
    Assert-True ($app.WaitForExit(5000) -and $app.ExitCode -eq 0) 'The relaunched instance failed to close.'

    [pscustomobject]@{
        Passed                         = $true
        Dpi                            = $dpi
        PanelPixels                    = ('{0}x{1}' -f $width, $height)
        TaskbarPixels                  = $dockedPixels
        DockHidden                     = [bool]$ExpectDockHidden
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
