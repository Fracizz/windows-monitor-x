# Windows 系统监控

面向 Windows 10 1703 及以上和 Windows 11 x64 的双行监控浮窗。运行 `windows-monitor.exe` 即可使用。

- 浮窗默认位于主屏工作区右下角。拖动可调整位置，靠近屏幕边缘时会吸附。
- 右键浮窗或托盘图标可切换主题、置顶、清零总流量和退出。单击托盘图标显示或隐藏浮窗。
- CPU、内存、磁盘和网速每秒更新。硬盘显示最忙物理磁盘的活动时间比例。采样数据不可用时显示破折号。
- 网速汇总在线的物理以太网和 Wi-Fi 网卡，包含局域网流量。总流量按工具运行期间的上传与下载累计，保存在 `%LOCALAPPDATA%\WindowsMonitor\state.ini`。

## 构建

Windows 上使用 Rust、MSVC Build Tools 和 Windows SDK。

```powershell
cargo build --release --locked
```

产物在 `target\release\windows-monitor.exe`。

## 验证

```powershell
cargo test --locked
cargo test --locked -- --ignored --nocapture
.\scripts\smoke.ps1
```

系统采样测试需要已启用的物理磁盘计数器。桌面冒烟测试需要登录到 Windows 桌面，覆盖浮窗尺寸、常用菜单操作、单实例、设置持久化与资源占用。
