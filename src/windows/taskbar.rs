use ::windows::{
    Win32::{
        Foundation::{HWND, RECT},
        Graphics::Gdi::{GetMonitorInfoW, MONITOR_DEFAULTTONULL, MONITORINFO, MonitorFromWindow},
        System::Com::{
            CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx,
            CoUninitialize,
        },
        UI::{
            Accessibility::*,
            Shell::{ABM_GETSTATE, ABS_AUTOHIDE, APPBARDATA, SHAppBarMessage},
            WindowsAndMessaging::{
                FindWindowExW, FindWindowW, GetDesktopWindow, GetForegroundWindow, GetShellWindow,
                GetWindowRect, IsWindowVisible,
            },
        },
    },
    core::{Interface, PCWSTR, Result, w},
};

use crate::model::Metrics;

use super::{panel::Layout, scale, state::TaskbarSide, window_dpi};

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum Placement {
    #[default]
    Floating,
    Hidden,
    Docked(RECT),
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

    pub fn placement(
        &mut self,
        own_window: HWND,
        side: TaskbarSide,
        metrics: Metrics,
    ) -> Placement {
        self.find_position(own_window, side, metrics)
            .unwrap_or(Placement::Floating)
    }

    fn find_position(
        &mut self,
        own_window: HWND,
        side: TaskbarSide,
        metrics: Metrics,
    ) -> Option<Placement> {
        let bar = unsafe { FindWindowW(w!("Shell_TrayWnd"), PCWSTR::null()) }.ok()?;
        let mut info = APPBARDATA {
            cbSize: size_of::<APPBARDATA>() as u32,
            ..Default::default()
        };
        if unsafe { SHAppBarMessage(ABM_GETSTATE, &mut info) } & ABS_AUTOHIDE as usize != 0 {
            return None;
        }
        let bounds = window_rect(bar)?;
        if bounds.right - bounds.left <= bounds.bottom - bounds.top {
            return None;
        }
        if !unsafe { IsWindowVisible(bar).as_bool() } || fullscreen_on_bar(bar, own_window) {
            return Some(Placement::Hidden);
        }
        let dpi = window_dpi(bar);
        let (width, height) = Layout::Taskbar.size(metrics);
        let (width, height) = (scale(width, dpi), scale(height, dpi));
        let padding = scale(3, dpi);
        if height + scale(4, dpi) > bounds.bottom - bounds.top {
            return None;
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
                if !valid_on_bar(rect, bounds) || rect.left >= tray.left {
                    continue;
                }
                let kind = child.CachedControlType().ok()?;
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
        let x = taskbar_gap(
            bounds.left,
            bounds.right,
            &mut self.occupied,
            width,
            padding,
            side,
        )?;
        let y = bounds.top + (bounds.bottom - bounds.top - height) / 2;
        Some(Placement::Docked(RECT {
            left: x,
            top: y,
            right: x + width,
            bottom: y + height,
        }))
    }
}

fn window_rect(hwnd: HWND) -> Option<RECT> {
    let mut rect = RECT::default();
    unsafe { GetWindowRect(hwnd, &mut rect) }.ok()?;
    Some(rect)
}

fn valid_on_bar(rect: RECT, bar: RECT) -> bool {
    rect.left < rect.right
        && rect.top < rect.bottom
        && rect.right > bar.left
        && rect.left < bar.right
        && rect.bottom > bar.top
        && rect.top < bar.bottom
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
        || foreground == bar
        || foreground == own_window
        || foreground == unsafe { GetShellWindow() }
        || foreground == unsafe { GetDesktopWindow() }
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
    let display = monitor.rcMonitor;
    window_rect(foreground).is_some_and(|rect| {
        rect.left <= display.left
            && rect.top <= display.top
            && rect.right >= display.right
            && rect.bottom >= display.bottom
    })
}

fn taskbar_gap(
    left: i32,
    right: i32,
    occupied: &mut [(i32, i32)],
    width: i32,
    padding: i32,
    side: TaskbarSide,
) -> Option<i32> {
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
            taskbar_gap(0, 1000, &mut occupied, 150, 6, TaskbarSide::Left),
            Some(71)
        );
        assert_eq!(
            taskbar_gap(0, 1000, &mut occupied, 150, 6, TaskbarSide::Right),
            Some(694)
        );
        for side in [TaskbarSide::Left, TaskbarSide::Right] {
            assert_eq!(taskbar_gap(0, 1000, &mut occupied, 350, 6, side), None);
        }
        let mut occupied = [(310, 650), (740, 999)];
        assert_eq!(
            taskbar_gap(0, 1000, &mut occupied, 240, 6, TaskbarSide::Right),
            Some(64)
        );
    }
}
