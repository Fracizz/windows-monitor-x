# windows-monitor-x

[简体中文](README.md) · English

A desktop monitor for 64-bit Windows 10 version 1703 or later and Windows 11. Run `windows-monitor-x.exe` to get started. The app runs with the current Windows user's permissions and shows a centered loading indicator while it starts.

## Usage

The menu labels below include the Chinese text shown in the app.

- The floating panel starts in the bottom-right corner of the primary display's work area. Drag it to move it. It snaps to nearby screen edges.
- Right-click the panel or the system tray icon and open Settings (设置) to configure startup, taskbar mode, always-on-top behavior, the refresh interval, and the theme. In Edit Metrics (编辑监控项), select the metrics to show, drag them to reorder them, and set the display size from 10 to 24 px (default 16 px). The size is based on 100% Windows scaling and follows the system DPI. You can preview both the floating and taskbar layouts. Click Apply (应用) to save your changes across restarts. The refresh interval can be set to 1, 2, 3, or 5 seconds and defaults to 1 second. The main context menu also lets you reset total traffic or exit. Click the tray icon to show or hide the panel.
- Select Transparent (透明) under Settings (设置) → Theme (主题) to reveal the background behind the metrics. Text automatically updates to white for a dark Windows taskbar and black for a light one. Clicks pass through transparent areas. Right-click the text or the tray icon to open the menu.
- In Edit Metrics → Density (编辑监控项 → 紧凑度), choose Standard (标准), Compact (紧凑), or Tight (更紧凑). This setting adjusts column spacing and side padding in both layouts.
- Enable Settings → Run at Sign-in (设置 → 开机自启) to start the monitor when the current Windows user signs in. This option is off by default. If you move the executable, use the same menu option to update the startup path.
- Enable Embed in Taskbar (嵌入任务栏) to fit the compact panel into available space on the primary display's horizontal taskbar. Under Settings → Taskbar Position (设置 → 任务栏位置), select Left (靠左) or Right (靠右). The default is Right. The panel floats when the taskbar has too little space, is set to auto-hide, or is vertical. The refresh interval, taskbar mode, and taskbar position are saved across restarts.
- Use Settings → Check for Updates (设置 → 检查更新) to check GitHub for the latest stable release. After confirmation, the app verifies the download's SHA-256 digest, replaces the executable, and restarts. Updating requires write access to the directory containing the executable. [GitHub Releases](https://github.com/Fracizz/windows-monitor-x/releases)

## Metrics

- By default, the panel displays upload and download speeds, CPU and memory usage, disk activity, and total network traffic. You can also enable available memory as a percentage, available and used memory in GB, current CPU frequency, system uptime, and battery level. Disk activity is the percentage of time the busiest physical disk is active. A dash indicates that a battery reading or other sample is unavailable.
- Percentages and transfer rates are displayed as integers. Physical memory in GB (1 GB = 1 billion bytes) and current CPU frequency in GHz are displayed with two decimal places. Both layouts use the same metrics, order, and display size, with items arranged from top to bottom in each column.
- Network speeds aggregate traffic from active physical Ethernet and Wi-Fi adapters, including local network traffic. Total traffic counts bytes uploaded and downloaded while the app is running and is saved in `%LOCALAPPDATA%\WindowsMonitor\state.ini`.

## Packaging

On Windows, build with Rust, MSVC Build Tools, and the Windows SDK.

```powershell
.\scripts\package.ps1
```

The script puts the executable in `dist` as `<repository-name>-x.exe`. The executable's product name, file description, and tray name use the same name without the file extension. The name comes from the Git `origin` remote when available or the local repository directory otherwise. Source archives use the Cargo package name. An existing `-x` or `-X` suffix is normalized to a single lowercase `-x`.

## Verification

```powershell
cargo test --locked
cargo test --locked -- --ignored --nocapture
.\scripts\smoke.ps1 -Exe .\dist\windows-monitor-x.exe
```

System sampling tests require enabled physical disk performance counters. The smoke test requires a signed-in Windows desktop session and covers floating panel dimensions, common menu actions, single-instance behavior, settings persistence, and resource usage.

With the desktop unlocked and enough space on the primary horizontal taskbar, use the following command to verify that the panel can embed, restore its embedded position after a restart, and return to floating mode.

```powershell
.\scripts\smoke.ps1 -Exe .\dist\windows-monitor-x.exe -RequireTaskbar
```
