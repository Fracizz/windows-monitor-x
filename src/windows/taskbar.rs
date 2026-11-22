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
                GUITHREADINFO, GW_HWNDPREV, GetAncestor, GetClassNameW, GetDesktopWindow,
                GetForegroundWindow, GetGUIThreadInfo, GetShellWindow, GetWindow, GetWindowRect,
                GetWindowThreadProcessId, IsWindowVisible,
            },
        },
    },
    core::{Interface, PCWSTR, Result, w},
};

use super::{scale, state::TaskbarSide, window_dpi};

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
}

#[derive(Default)]
pub struct DockPlacement {
    current: Option<Placement>,
    pending: Option<(Placement, Instant)>,
    preferred_x: Option<i32>,
}

impl DockPlacement {
    pub fn preferred_x(&self) -> Option<i32> {
        self.preferred_x
    }

    pub fn reset_preference(&mut self) {
        self.preferred_x = None;
        self.pending = None;
    }

    pub fn hold(&mut self) -> Option<Placement> {
        self.pending = None;
        self.current
    }

    pub fn resolve(&mut self, observed: Option<Placement>, now: Instant) -> Option<Placement> {
        let next = observed.unwrap_or_default();
        // Explorer's layout and foreground bounds can be in transition for one poll.
        let needs_confirmation = Some(next) != self.current
            && (observed.is_none()
                || next == Placement::Hidden
                || matches!(
                    (self.current, next),
                    (Some(Placement::Docked(_)), Placement::Docked(_))
                ));
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
        self.pending = None;
        self.current = Some(next);
        if let Placement::Docked(rect) = next {
            self.preferred_x = Some(rect.left);
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
        (width, height): (i32, i32),
        preferred_x: Option<i32>,
    ) -> Option<Observation> {
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
        let (width, height) = (scale(width, dpi), scale(height, dpi));
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
        self.occupied.push((tray.left, bounds.right));

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
            bounds.right,
            &mut self.occupied,
            width,
            padding,
            side,
            preferred_x,
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
    if foreground.is_invalid()
        || foreground == own_window
        || foreground == unsafe { GetShellWindow() }
        || foreground == unsafe { GetDesktopWindow() }
        || is_taskbar_window(bar, foreground)
    {
        return false;
    }
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
    if unsafe {
        DwmGetWindowAttribute(
            foreground,
            DWMWA_EXTENDED_FRAME_BOUNDS,
            (&raw mut frame).cast(),
            size_of::<RECT>() as u32,
        )
    }
    .is_err()
    {
        return false;
    }
    let display = monitor.rcMonitor;
    frame.left <= display.left
        && frame.top <= display.top
        && frame.right >= display.right
        && frame.bottom >= display.bottom
}

fn taskbar_gap(
    left: i32,
    right: i32,
    occupied: &mut [(i32, i32)],
    width: i32,
    padding: i32,
    side: TaskbarSide,
    preferred_x: Option<i32>,
) -> Option<i32> {
    if let Some(x) = preferred_x {
        let (start, end) = (x - padding, x + width + padding);
        if start >= left && end <= right && occupied.iter().all(|&(a, b)| b <= start || a >= end) {
            return Some(x);
        }
    }
    occupied.sort_unstable();
    let mut found = None;
    let mut next = left;
    for (start, end) in occupied.iter().copied().chain([(right, right)]) {
        let start = start.clamp(left, right);
        if start - next >= width + 2 * padding {
            if side == TaskbarSide::Left {
                return Some(next + padding);
            }
            found = Some(start - padding - width);
        }
        next = next.max(end.clamp(left, right));
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn docking_prefers_the_selected_edge_and_keeps_clear_of_controls() {
        let mut occupied = [(850, 1000), (15, 65), (300, 440), (390, 500)];
        assert_eq!(
            taskbar_gap(0, 1000, &mut occupied, 150, 6, TaskbarSide::Left, None),
            Some(71)
        );
        assert_eq!(
            taskbar_gap(0, 1000, &mut occupied, 150, 6, TaskbarSide::Right, None),
            Some(694)
        );
        for side in [TaskbarSide::Left, TaskbarSide::Right] {
            assert_eq!(
                taskbar_gap(0, 1000, &mut occupied, 350, 6, side, None),
                None
            );
        }
        let mut occupied = [(310, 650), (740, 999)];
        assert_eq!(
            taskbar_gap(0, 1000, &mut occupied, 240, 6, TaskbarSide::Right, None),
            Some(64)
        );
    }

    #[test]
    fn reuses_the_docked_slot_until_a_control_encroaches() {
        let mut occupied = [(0, 60), (400, 480), (820, 1000)];
        assert_eq!(
            taskbar_gap(0, 1000, &mut occupied, 150, 6, TaskbarSide::Left, Some(200)),
            Some(200)
        );
        occupied[1] = (355, 480);
        assert_eq!(
            taskbar_gap(0, 1000, &mut occupied, 150, 6, TaskbarSide::Left, Some(200)),
            Some(66)
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
        assert_eq!(placement.preferred_x(), Some(80));
        assert_eq!(placement.resolve(Some(moved), start + second * 5), None);
        assert_eq!(
            placement.resolve(Some(moved), start + second * 6),
            Some(moved)
        );
        assert_eq!(placement.preferred_x(), Some(280));
        assert_eq!(
            placement.resolve(Some(Placement::Hidden), start + second * 7),
            None
        );
        assert_eq!(
            placement.resolve(Some(Placement::Hidden), start + second * 8),
            Some(Placement::Hidden)
        );
        assert_eq!(placement.preferred_x(), Some(280));
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
