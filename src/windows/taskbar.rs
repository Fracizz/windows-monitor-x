use std::{
    collections::HashSet,
    time::{Duration, Instant},
};

use ::windows::{
    Win32::{
        Foundation::{HWND, RECT},
        Graphics::{
            Dwm::{DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute},
            Gdi::{GetMonitorInfoW, MONITOR_DEFAULTTONULL, MONITORINFO, MonitorFromWindow},
        },
        System::Com::{
            CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx,
            CoUninitialize,
        },
        UI::{
            Accessibility::*,
            Shell::{ABM_GETSTATE, ABS_AUTOHIDE, APPBARDATA, SHAppBarMessage},
            WindowsAndMessaging::{
                FindWindowExW, FindWindowW, GA_ROOTOWNER, GUI_INMENUMODE, GUI_POPUPMENUMODE,
                GUITHREADINFO, GW_HWNDPREV, GW_OWNER, GWL_EXSTYLE, GetAncestor, GetClassNameW,
                GetDesktopWindow, GetForegroundWindow, GetGUIThreadInfo, GetShellWindow, GetWindow,
                GetWindowLongPtrW, GetWindowRect, GetWindowThreadProcessId, IsWindowVisible,
                WS_EX_APPWINDOW, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST,
            },
        },
    },
    core::{Interface, PCWSTR, Result, w},
};

use super::{panel::LayoutSize, scale, state::TaskbarSide, window_dpi};

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum Placement {
    #[default]
    Floating,
    Hidden,
    Docked(RECT),
}

pub enum Observation {
    Placement(Placement),
    NoRoom,
    MenuOpen,
    /// 截图工具等全屏置顶覆盖层在前台：任务栏读数不可信，保持当前位置。
    Overlay,
}

#[derive(Default)]
pub struct DockPlacement {
    current: Option<Placement>,
    pending: Option<(Placement, Instant)>,
    /// 最近一次确认的贴边槽位，从隐藏恢复时优先回到这里。
    docked: Option<RECT>,
}

impl DockPlacement {
    pub fn reset_pending(&mut self) {
        self.pending = None;
    }

    pub fn hold(&mut self) -> Option<Placement> {
        self.pending = None;
        self.current
    }

    pub fn resolve(&mut self, observed: Option<Placement>, now: Instant) -> Option<Placement> {
        let next = observed.unwrap_or_default();
        // Explorer's layout and foreground bounds can be in transition for one poll.
        // 离开贴边槽位（换槽、转浮窗、隐藏）都要持续 1 秒才生效，避免一次异常读数让面板跳走。
        let needs_confirmation = Some(next) != self.current
            && (observed.is_none()
                || next == Placement::Hidden
                || matches!(self.current, Some(Placement::Docked(_))));
        if needs_confirmation {
            let since = match self.pending {
                Some((candidate, since)) if candidate == next => since,
                _ => {
                    self.pending = Some((next, now));
                    now
                }
            };
            if now.duration_since(since) < Duration::from_secs(1) {
                return None;
            }
        }
        // 从隐藏（全屏应用、截图后）恢复时任务栏布局可能还在变化：先回到原槽位，
        // 新槽位按换槽规则持续 1 秒后才采用，避免出现后再跳一次。
        if self.current == Some(Placement::Hidden)
            && let Placement::Docked(rect) = next
            && let Some(previous) = self.docked
            && previous != rect
        {
            self.pending = Some((next, now));
            self.current = Some(Placement::Docked(previous));
            return self.current;
        }
        self.pending = None;
        self.current = Some(next);
        if let Placement::Docked(rect) = next {
            self.docked = Some(rect);
        }
        Some(next)
    }
}

struct Apartment;

impl Drop for Apartment {
    fn drop(&mut self) {
        unsafe { CoUninitialize() };
    }
}

pub struct Taskbar {
    automation: IUIAutomation,
    controls: IUIAutomationCondition,
    cache: IUIAutomationCacheRequest,
    occupied: Vec<(i32, i32)>,
    _apartment: Apartment,
}

impl Taskbar {
    pub fn new() -> Result<Self> {
        unsafe {
            CoInitializeEx(None, COINIT_MULTITHREADED).ok()?;
            let apartment = Apartment;
            let automation: IUIAutomation =
                CoCreateInstance(&CUIAutomation8, None, CLSCTX_INPROC_SERVER)?;
            let timeouts = automation.cast::<IUIAutomation2>()?;
            timeouts.SetConnectionTimeout(250)?;
            timeouts.SetTransactionTimeout(350)?;
            let controls = automation.ControlViewCondition()?;
            let cache = automation.CreateCacheRequest()?;
            cache.SetAutomationElementMode(AutomationElementMode_None)?;
            for property in [
                UIA_BoundingRectanglePropertyId,
                UIA_IsOffscreenPropertyId,
                UIA_ControlTypePropertyId,
                UIA_ClassNamePropertyId,
                UIA_NativeWindowHandlePropertyId,
            ] {
                cache.AddProperty(property)?;
            }
            Ok(Self {
                automation,
                controls,
                cache,
                occupied: Vec::new(),
                _apartment: apartment,
            })
        }
    }

    pub fn observe(
        &mut self,
        bar: HWND,
        own_window: HWND,
        side: TaskbarSide,
        size: LayoutSize,
    ) -> Option<Observation> {
        if overlay_on_bar(bar, own_window) {
            return Some(Observation::Overlay);
        }
        let mut info = APPBARDATA {
            cbSize: size_of::<APPBARDATA>() as u32,
            ..Default::default()
        };
        if unsafe { SHAppBarMessage(ABM_GETSTATE, &mut info) } & ABS_AUTOHIDE as usize != 0 {
            return Some(Observation::Placement(Placement::Floating));
        }
        let bounds = window_rect(bar)?;
        if bounds.right - bounds.left <= bounds.bottom - bounds.top {
            return Some(Observation::Placement(Placement::Floating));
        }
        if !unsafe { IsWindowVisible(bar).as_bool() } || fullscreen_on_bar(bar, own_window) {
            return Some(Observation::Placement(Placement::Hidden));
        }
        let dpi = window_dpi(bar);
        let (width, height) = size.pixels(dpi);
        let padding = scale(3, dpi);
        if height + scale(4, dpi) > bounds.bottom - bounds.top {
            return Some(Observation::Placement(Placement::Floating));
        }

        let tray =
            unsafe { FindWindowExW(Some(bar), None, w!("TrayNotifyWnd"), PCWSTR::null()) }.ok()?;
        let tray = window_rect(tray)?;
        if !valid_on_bar(tray, bounds) || tray.left <= bounds.left {
            return None;
        }
        self.occupied.clear();

        let mut has_app_buttons = false;
        unsafe {
            let root = self.automation.ElementFromHandle(bar).ok()?;
            let elements = root
                .FindAllBuildCache(TreeScope_Descendants, &self.controls, &self.cache)
                .ok()?;
            for index in 0..elements.Length().ok()? {
                let child = elements.GetElement(index).ok()?;
                if child.CachedIsOffscreen().ok()?.as_bool() {
                    continue;
                }
                let rect = child.CachedBoundingRectangle().ok()?;
                let kind = child.CachedControlType().ok()?;
                if popup_menu(kind, rect, bounds) {
                    return Some(Observation::MenuOpen);
                }
                if !valid_on_bar(rect, bounds) || rect.left >= tray.left {
                    continue;
                }
                if is_button(kind) {
                    has_app_buttons = true;
                } else if kind != UIA_PaneControlTypeId
                    || child.CachedNativeWindowHandle().ok()?.0.is_null()
                {
                    continue;
                } else {
                    // The XAML input host covers empty space as well as buttons. Use its children.
                    let class = child.CachedClassName().ok()?;
                    if matches!(
                        class.to_string().as_str(),
                        "Windows.UI.Input.InputSite.WindowClass"
                            | "Windows.UI.Composition.DesktopWindowContentBridge"
                    ) {
                        continue;
                    }
                }
                self.occupied.push((rect.left, rect.right));
            }
        }
        if !has_app_buttons {
            return None;
        }
        let Some(x) = taskbar_gap(
            bounds.left,
            tray.left,
            &mut self.occupied,
            width,
            padding,
            side,
        ) else {
            return Some(Observation::NoRoom);
        };
        let y = bounds.top + (bounds.bottom - bounds.top - height) / 2;
        Some(Observation::Placement(Placement::Docked(RECT {
            left: x,
            top: y,
            right: x + width,
            bottom: y + height,
        })))
    }
}

pub fn primary_taskbar() -> Option<HWND> {
    unsafe { FindWindowW(w!("Shell_TrayWnd"), PCWSTR::null()) }.ok()
}

pub fn taskbar_has_focus(bar: HWND) -> bool {
    is_taskbar_window(bar, unsafe { GetForegroundWindow() })
}

pub fn is_taskbar_window(bar: HWND, window: HWND) -> bool {
    window == bar
        || unsafe { GetAncestor(window, GA_ROOTOWNER) == bar }
        || is_tray_overflow(bar, window)
}

pub fn is_tray_overflow(bar: HWND, window: HWND) -> bool {
    let mut class = [0; 64];
    let len = unsafe { GetClassNameW(window, &mut class) } as usize;
    if ![
        w!("TopLevelWindowForOverflowXamlIsland"),
        w!("NotifyIconOverflowWindow"),
    ]
    .iter()
    .any(|name| unsafe { name.as_wide() } == &class[..len])
    {
        return false;
    }
    let (mut shell_pid, mut window_pid) = (0, 0);
    unsafe {
        GetWindowThreadProcessId(bar, Some(&mut shell_pid));
        GetWindowThreadProcessId(window, Some(&mut window_pid));
    }
    shell_pid != 0 && shell_pid == window_pid
}

pub fn taskbar_menu_open(bar: HWND) -> bool {
    let thread = unsafe { GetWindowThreadProcessId(bar, None) };
    let mut info = GUITHREADINFO {
        cbSize: size_of::<GUITHREADINFO>() as u32,
        ..Default::default()
    };
    thread != 0
        && unsafe { GetGUIThreadInfo(thread, &mut info) }.is_ok()
        && (info.flags & (GUI_INMENUMODE | GUI_POPUPMENUMODE)).0 != 0
}

pub fn covered_by_taskbar(bar: HWND, window: HWND, rect: RECT) -> bool {
    if !unsafe { IsWindowVisible(window).as_bool() } || window_rect(window) != Some(rect) {
        return false;
    }
    if !unsafe { IsWindowVisible(bar).as_bool() }
        || !window_rect(bar).is_some_and(|bounds| valid_on_bar(rect, bounds))
    {
        return false;
    }
    // The transparent panel's input window can be above the taskbar while its visual is below.
    let mut visited = HashSet::new();
    let mut current = window;
    while let Ok(above) = unsafe { GetWindow(current, GW_HWNDPREV) } {
        if above == bar {
            return true;
        }
        if !visited.insert(above.0) {
            break;
        }
        current = above;
    }
    false
}

fn window_rect(hwnd: HWND) -> Option<RECT> {
    let mut rect = RECT::default();
    unsafe { GetWindowRect(hwnd, &mut rect) }.ok()?;
    Some(rect)
}

fn valid_on_bar(rect: RECT, bar: RECT) -> bool {
    let row_center = bar.top + (bar.bottom - bar.top) / 2;
    rect.left < rect.right
        && rect.right > bar.left
        && rect.left < bar.right
        && rect.top <= row_center
        && rect.bottom > row_center
}

fn popup_menu(kind: UIA_CONTROLTYPE_ID, rect: RECT, bar: RECT) -> bool {
    (kind == UIA_MenuControlTypeId || kind == UIA_MenuItemControlTypeId)
        && rect.left < rect.right
        && rect.top < rect.bottom
        && rect.right > bar.left
        && rect.left < bar.right
        && (rect.top < bar.top || rect.bottom > bar.bottom)
}

fn is_button(kind: UIA_CONTROLTYPE_ID) -> bool {
    [
        UIA_ButtonControlTypeId,
        UIA_CheckBoxControlTypeId,
        UIA_ComboBoxControlTypeId,
        UIA_EditControlTypeId,
        UIA_HyperlinkControlTypeId,
        UIA_ListItemControlTypeId,
        UIA_MenuItemControlTypeId,
        UIA_SplitButtonControlTypeId,
        UIA_TabItemControlTypeId,
    ]
    .contains(&kind)
}

fn fullscreen_on_bar(bar: HWND, own_window: HWND) -> bool {
    let foreground = unsafe { GetForegroundWindow() };
    foreground_candidate(bar, own_window, foreground)
        && !screen_overlay(foreground)
        && covers_bar_monitor(bar, foreground)
}

/// 前台是否为盖住任务栏所在显示器的截图覆盖层。截图期间（以及刚结束时）任务栏的
/// 可见性和布局读数都不可信，此时保持面板原状，不隐藏、不换槽、不转浮窗。
fn overlay_on_bar(bar: HWND, own_window: HWND) -> bool {
    let foreground = unsafe { GetForegroundWindow() };
    foreground_candidate(bar, own_window, foreground)
        && screen_overlay(foreground)
        && covers_bar_monitor(bar, foreground)
}

fn foreground_candidate(bar: HWND, own_window: HWND, foreground: HWND) -> bool {
    !(foreground.is_invalid()
        || foreground == own_window
        || foreground == unsafe { GetShellWindow() }
        || foreground == unsafe { GetDesktopWindow() }
        || is_taskbar_window(bar, foreground))
}

fn covers_bar_monitor(bar: HWND, window: HWND) -> bool {
    let mut monitor = MONITORINFO {
        cbSize: size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    let found =
        unsafe { GetMonitorInfoW(MonitorFromWindow(bar, MONITOR_DEFAULTTONULL), &mut monitor) };
    if !found.as_bool() {
        return false;
    }
    let mut frame = RECT::default();
    let dwm = unsafe {
        DwmGetWindowAttribute(
            window,
            DWMWA_EXTENDED_FRAME_BOUNDS,
            (&raw mut frame).cast(),
            size_of::<RECT>() as u32,
        )
    };
    // 分层/无边框覆盖层可能取不到 DWM 边框，退回窗口矩形。
    let Some(frame) = dwm.is_ok().then_some(frame).or_else(|| window_rect(window)) else {
        return false;
    };
    let display = monitor.rcMonitor;
    frame.left <= display.left
        && frame.top <= display.top
        && frame.right >= display.right
        && frame.bottom >= display.bottom
}

/// 截图工具（PixPin、Snipaste、微信/QQ 截图等）打开时会盖一层全屏置顶、不在任务栏显示按钮的窗口，
/// 它不是全屏应用，不能据此隐藏面板，否则面板会在截图开始时消失、结束时又出现，看起来像闪烁。
/// 全屏游戏、视频等通常有任务栏按钮（非工具窗口、无所有者），仍按全屏处理。
fn screen_overlay(window: HWND) -> bool {
    let style = unsafe { GetWindowLongPtrW(window, GWL_EXSTYLE) };
    if style & WS_EX_TOPMOST.0 as isize == 0 || style & WS_EX_APPWINDOW.0 as isize != 0 {
        return false;
    }
    let no_button = (WS_EX_TOOLWINDOW.0 | WS_EX_NOACTIVATE.0) as isize;
    style & no_button != 0
        || unsafe { GetWindow(window, GW_OWNER) }.is_ok_and(|owner| !owner.is_invalid())
}

fn taskbar_gap(
    left: i32,
    tray_left: i32,
    occupied: &mut [(i32, i32)],
    width: i32,
    padding: i32,
    side: TaskbarSide,
) -> Option<i32> {
    let left = if side == TaskbarSide::Right {
        // The right slot starts after the last visible taskbar control.
        occupied.iter().fold(left, |edge, &(_, end)| edge.max(end))
    } else {
        left
    };
    // 始终按所选方向计算贴边槽位，不沿用旧位置：
    // Explorer 短暂布局变化（防抖 1 秒由 DockPlacement 负责）若把面板挤到别处，
    // 沿用旧位置会让它永远停在那里，出现“设置靠左、实际靠右”。
    if side == TaskbarSide::Right {
        return (tray_left - left >= width + 2 * padding).then_some(tray_left - padding - width);
    }

    occupied.sort_unstable();
    let mut next = left;
    for (start, end) in occupied.iter().copied().chain([(tray_left, tray_left)]) {
        let start = start.clamp(left, tray_left);
        if start - next >= width + 2 * padding {
            return Some(next + padding);
        }
        next = next.max(end.clamp(left, tray_left));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn docking_prefers_the_selected_edge_and_keeps_clear_of_controls() {
        let mut occupied = [(15, 65), (390, 500), (300, 440)];
        assert_eq!(
            taskbar_gap(0, 850, &mut occupied, 150, 6, TaskbarSide::Left),
            Some(71)
        );
        assert_eq!(
            taskbar_gap(0, 850, &mut occupied, 150, 6, TaskbarSide::Right),
            Some(694)
        );
        for side in [TaskbarSide::Left, TaskbarSide::Right] {
            assert_eq!(taskbar_gap(0, 850, &mut occupied, 350, 6, side), None);
        }
    }

    #[test]
    fn right_docking_waits_for_room_next_to_the_tray() {
        let mut occupied = [(310, 650)];
        assert_eq!(
            taskbar_gap(0, 740, &mut occupied, 240, 6, TaskbarSide::Left),
            Some(6)
        );
        assert_eq!(
            taskbar_gap(0, 740, &mut occupied, 240, 6, TaskbarSide::Right),
            None
        );
        occupied[0].1 = 488;
        assert_eq!(
            taskbar_gap(0, 740, &mut occupied, 240, 6, TaskbarSide::Right),
            Some(494)
        );
    }

    #[test]
    fn left_docking_returns_to_the_edge_once_a_transient_control_leaves() {
        // 短暂出现的控件把面板挤到中间后，控件消失应回到左侧，而不是停在旧位置。
        let mut occupied = [(0, 60), (400, 480)];
        assert_eq!(
            taskbar_gap(0, 820, &mut occupied, 150, 6, TaskbarSide::Left),
            Some(66)
        );
        let mut occupied = [(0, 360), (400, 480)];
        assert_eq!(
            taskbar_gap(0, 820, &mut occupied, 150, 6, TaskbarSide::Left),
            Some(486)
        );
    }

    #[test]
    fn confirms_layout_and_visibility_changes_before_leaving_a_slot() {
        let start = Instant::now();
        let second = Duration::from_secs(1);
        let rect = RECT {
            left: 80,
            top: 800,
            right: 230,
            bottom: 836,
        };
        let docked = Placement::Docked(rect);
        let moved = Placement::Docked(RECT {
            left: 280,
            right: 430,
            ..rect
        });
        let mut placement = DockPlacement::default();

        assert_eq!(placement.resolve(None, start), None);
        assert_eq!(
            placement.resolve(Some(docked), start + second / 2),
            Some(docked)
        );
        assert_eq!(placement.resolve(None, start + second), None);
        assert_eq!(
            placement.resolve(Some(Placement::Hidden), start + second * 2),
            None
        );
        assert_eq!(placement.resolve(Some(moved), start + second * 3), None);
        assert_eq!(
            placement.resolve(Some(docked), start + second * 4),
            Some(docked)
        );

        assert_eq!(placement.resolve(Some(moved), start + second * 5), None);
        assert_eq!(placement.hold(), Some(docked));
        assert_eq!(placement.resolve(Some(moved), start + second * 5), None);
        assert_eq!(
            placement.resolve(Some(moved), start + second * 6),
            Some(moved)
        );
        assert_eq!(
            placement.resolve(Some(Placement::Hidden), start + second * 7),
            None
        );
        assert_eq!(
            placement.resolve(Some(Placement::Hidden), start + second * 8),
            Some(Placement::Hidden)
        );
        assert_eq!(
            placement.resolve(Some(moved), start + second * 9),
            Some(moved)
        );
        assert_eq!(placement.resolve(None, start + second * 10), None);
        assert_eq!(
            placement.resolve(None, start + second * 11),
            Some(Placement::Floating)
        );
    }

    #[test]
    fn leaving_a_slot_for_floating_needs_confirmation() {
        let start = Instant::now();
        let second = Duration::from_secs(1);
        let docked = Placement::Docked(RECT {
            left: 80,
            top: 800,
            right: 230,
            bottom: 836,
        });
        let mut placement = DockPlacement::default();
        assert_eq!(placement.resolve(Some(docked), start), Some(docked));
        assert_eq!(
            placement.resolve(Some(Placement::Floating), start + second / 2),
            None
        );
        assert_eq!(
            placement.resolve(Some(docked), start + second),
            Some(docked)
        );
        assert_eq!(
            placement.resolve(Some(Placement::Floating), start + second * 2),
            None
        );
        assert_eq!(
            placement.resolve(Some(Placement::Floating), start + second * 3),
            Some(Placement::Floating)
        );
    }

    #[test]
    fn returning_from_hidden_reuses_the_previous_slot_first() {
        let start = Instant::now();
        let second = Duration::from_secs(1);
        let rect = RECT {
            left: 80,
            top: 800,
            right: 230,
            bottom: 836,
        };
        let docked = Placement::Docked(rect);
        let shifted = Placement::Docked(RECT {
            left: 280,
            right: 430,
            ..rect
        });
        let mut placement = DockPlacement::default();
        assert_eq!(placement.resolve(Some(docked), start), Some(docked));
        assert_eq!(placement.resolve(Some(Placement::Hidden), start), None);
        assert_eq!(
            placement.resolve(Some(Placement::Hidden), start + second),
            Some(Placement::Hidden)
        );
        // 截图/全屏结束后的第一次读数槽位偏了：先回原位，不跳。
        assert_eq!(
            placement.resolve(Some(shifted), start + second * 2),
            Some(docked)
        );
        // 偏移只是瞬时的：回到原槽位后不再变化。
        assert_eq!(
            placement.resolve(Some(docked), start + second * 2 + second / 2),
            Some(docked)
        );
        // 新槽位持续 1 秒才采用。
        assert_eq!(placement.resolve(Some(shifted), start + second * 3), None);
        assert_eq!(
            placement.resolve(Some(shifted), start + second * 4),
            Some(shifted)
        );
    }

    #[test]
    fn topmost_tool_windows_are_treated_as_screen_overlays() {
        use ::windows::Win32::UI::WindowsAndMessaging::{
            CreateWindowExW, DestroyWindow, WINDOW_EX_STYLE, WS_POPUP,
        };
        let create = |style: WINDOW_EX_STYLE| unsafe {
            CreateWindowExW(
                style,
                w!("Static"),
                PCWSTR::null(),
                WS_POPUP,
                0,
                0,
                1,
                1,
                None,
                None,
                None,
                None,
            )
            .unwrap()
        };
        for (style, expected) in [
            (WS_EX_TOPMOST | WS_EX_TOOLWINDOW, true),
            (WS_EX_TOPMOST | WS_EX_NOACTIVATE, true),
            (WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_APPWINDOW, false),
            (WS_EX_TOPMOST, false),
            (WS_EX_TOOLWINDOW, false),
            (WINDOW_EX_STYLE::default(), false),
        ] {
            let window = create(style);
            assert_eq!(screen_overlay(window), expected);
            let _ = unsafe { DestroyWindow(window) };
        }
    }

    #[test]
    fn a_popup_touching_the_taskbar_edge_does_not_occupy_a_slot() {
        let bar = RECT {
            left: 0,
            top: 800,
            right: 1000,
            bottom: 840,
        };
        let popup = RECT {
            left: 30,
            top: 730,
            right: 180,
            bottom: 806,
        };
        let button = RECT {
            left: 30,
            top: 806,
            right: 80,
            bottom: 836,
        };
        assert!(!valid_on_bar(popup, bar));
        assert!(valid_on_bar(button, bar));
        assert!(popup_menu(UIA_MenuItemControlTypeId, popup, bar));
        assert!(!popup_menu(UIA_MenuItemControlTypeId, button, bar));
        assert!(!popup_menu(UIA_ButtonControlTypeId, popup, bar));
    }
}
