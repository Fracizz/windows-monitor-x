//! 浮窗贴边收起：拖到屏幕边缘后贴齐，鼠标离开后最小化成贴边的小胶囊（显示 CPU 使用率），鼠标移入时展开。
//!
//! 贴边状态不单独写入设置：贴边时保存的位置与工作区边缘齐平（普通吸附会留出边距），
//! 重启或重新布局后据此恢复。

use super::*;

pub(super) const EDGE_TIMER: usize = 3;
/// 轮询鼠标位置的间隔（毫秒）。
const EDGE_POLL: u32 = 100;
/// 鼠标离开后连续多少次轮询仍在窗口外才收起（约 400ms），避免划过时抖动。
const HIDE_AFTER_TICKS: u8 = 6;
/// 拖到边缘松手后多少次轮询自动收起（约 800ms）。
const COLLAPSE_AFTER_DOCK_TICKS: u8 = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Edge {
    Left,
    Top,
    Right,
    Bottom,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct EdgeDock {
    edge: Edge,
    /// 展开时的窗口矩形；收起成胶囊后据此还原。
    shown: RECT,
    hidden: bool,
    away: u8,
    /// 拖到边缘松手后立即收起时，鼠标可能正好落在胶囊上；先等鼠标离开一次再响应悬停展开，避免立刻弹开。
    wait_leave: bool,
    /// 刚贴边时的收起倒计时（轮询次数）；到 0 时无论鼠标位置都收起。
    collapse_in: Option<u8>,
}

fn monitor_info(rect: &RECT) -> Option<MONITORINFO> {
    let center = POINT {
        x: rect.left + (rect.right - rect.left) / 2,
        y: rect.top + (rect.bottom - rect.top) / 2,
    };
    let mut info = MONITORINFO {
        cbSize: size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    unsafe {
        GetMonitorInfoW(
            MonitorFromPoint(center, MONITOR_DEFAULTTONEAREST),
            &mut info,
        )
    }
    .as_bool()
    .then_some(info)
}

/// 外侧没有相邻显示器的边缘才能贴边（胶囊停在工作区内，任务栏一侧也可以）。
fn edge_allowed(edge: Edge, info: &MONITORINFO, rect: &RECT) -> bool {
    let full = info.rcMonitor;
    let mid_x = ((rect.left + rect.right) / 2).clamp(full.left, full.right - 1);
    let mid_y = ((rect.top + rect.bottom) / 2).clamp(full.top, full.bottom - 1);
    let outside = match edge {
        Edge::Left => POINT {
            x: full.left - 1,
            y: mid_y,
        },
        Edge::Right => POINT {
            x: full.right,
            y: mid_y,
        },
        Edge::Top => POINT {
            x: mid_x,
            y: full.top - 1,
        },
        Edge::Bottom => POINT {
            x: mid_x,
            y: full.bottom,
        },
    };
    unsafe { MonitorFromPoint(outside, MONITOR_DEFAULTTONULL) }.is_invalid()
}

/// 窗口越过（`overshoot >= 0`）或恰好齐平（`overshoot == 0`）的可收起边缘，取越过最多的一条。
fn edge_of(rect: RECT, accept: impl Fn(i32) -> bool) -> Option<Edge> {
    let info = monitor_info(&rect)?;
    let work = info.rcWork;
    [
        (Edge::Left, work.left - rect.left),
        (Edge::Right, rect.right - work.right),
        (Edge::Top, work.top - rect.top),
        (Edge::Bottom, rect.bottom - work.bottom),
    ]
    .into_iter()
    .filter(|&(edge, overshoot)| accept(overshoot) && edge_allowed(edge, &info, &rect))
    .max_by_key(|&(_, overshoot)| overshoot)
    .map(|(edge, _)| edge)
}

/// 拖动结束时窗口靠近（距离小于 `threshold`）、碰到或越过了哪条可收起的边缘。
pub(super) fn dragged_edge(rect: RECT, threshold: i32) -> Option<Edge> {
    edge_of(rect, |overshoot| overshoot > -threshold)
}

/// 与边缘齐平时窗口应处的位置：贴边方向贴齐工作区，另一方向限制在工作区内。
fn flush_position(edge: Edge, rect: RECT) -> Option<POINT> {
    let work = monitor_info(&rect)?.rcWork;
    let (width, height) = (rect.right - rect.left, rect.bottom - rect.top);
    let max_x = (work.right - width).max(work.left);
    let max_y = (work.bottom - height).max(work.top);
    let mut position = POINT {
        x: rect.left.clamp(work.left, max_x),
        y: rect.top.clamp(work.top, max_y),
    };
    match edge {
        Edge::Left => position.x = work.left,
        Edge::Right => position.x = max_x,
        Edge::Top => position.y = work.top,
        Edge::Bottom => position.y = max_y,
    }
    Some(position)
}

/// 收起后小胶囊的矩形：贴着工作区边缘，沿边缘方向与面板中心对齐。
fn capsule_rect(edge: Edge, rect: RECT, dpi: u32) -> Option<RECT> {
    let work = monitor_info(&rect)?.rcWork;
    // 左右边缘用竖直胶囊，上下边缘用水平胶囊。
    let (long, short) = (
        scale(panel::CAPSULE_LENGTH, dpi),
        scale(panel::CAPSULE_THICKNESS, dpi),
    );
    let (width, height) = if is_vertical(edge) {
        (short, long)
    } else {
        (long, short)
    };
    let center_x = (rect.left + rect.right) / 2;
    let center_y = (rect.top + rect.bottom) / 2;
    let x = (center_x - width / 2).clamp(work.left, (work.right - width).max(work.left));
    let y = (center_y - height / 2).clamp(work.top, (work.bottom - height).max(work.top));
    let (left, top) = match edge {
        Edge::Left => (work.left, y),
        Edge::Right => (work.right - width, y),
        Edge::Top => (x, work.top),
        Edge::Bottom => (x, work.bottom - height),
    };
    Some(RECT {
        left,
        top,
        right: left + width,
        bottom: top + height,
    })
}

fn place_rect(hwnd: HWND, rect: RECT) {
    let _ = unsafe {
        SetWindowPos(
            hwnd,
            Some(HWND_TOPMOST),
            rect.left,
            rect.top,
            rect.right - rect.left,
            rect.bottom - rect.top,
            SWP_NOACTIVATE,
        )
    };
    invalidate(hwnd);
}
fn move_to(hwnd: HWND, position: POINT) {
    let _ = unsafe {
        SetWindowPos(
            hwnd,
            Some(HWND_TOPMOST),
            position.x,
            position.y,
            0,
            0,
            SWP_NOSIZE | SWP_NOACTIVATE,
        )
    };
}

fn start(hwnd: HWND, edge: Edge) {
    let mut shown = RECT::default();
    let _ = unsafe { GetWindowRect(hwnd, &mut shown) };
    with_app(|app| {
        app.edge_dock = Some(EdgeDock {
            edge,
            shown,
            hidden: false,
            away: 0,
            wait_leave: false,
            collapse_in: None,
        });
    });
    set_topmost(hwnd, true);
    unsafe { SetTimer(Some(hwnd), EDGE_TIMER, EDGE_POLL, None) };
}

/// 拖到边缘后贴齐并进入贴边收起状态，返回保存的位置。
pub(super) fn dock(hwnd: HWND, edge: Edge) -> Option<POINT> {
    let mut rect = RECT::default();
    unsafe { GetWindowRect(hwnd, &mut rect) }.ok()?;
    let position = flush_position(edge, rect)?;
    move_to(hwnd, position);
    start(hwnd, edge);
    // 松手后稍等片刻再收起（不要求鼠标先离开），避免一贴边就突兀地缩掉。
    with_app(|app| {
        if let Some(dock) = &mut app.edge_dock {
            dock.collapse_in = Some(COLLAPSE_AFTER_DOCK_TICKS);
        }
    });
    Some(position)
}

pub(super) fn undock(hwnd: HWND) {
    if with_app(|app| app.edge_dock.take()).flatten().is_some() {
        let _ = unsafe { KillTimer(Some(hwnd), EDGE_TIMER) };
    }
}

/// 浮窗重新布局后，若位置与可收起的边缘齐平则恢复贴边状态，否则退出贴边。
pub(super) fn restore(hwnd: HWND) {
    let mut rect = RECT::default();
    let edge = unsafe { GetWindowRect(hwnd, &mut rect) }
        .is_ok()
        .then(|| edge_of(rect, |overshoot| overshoot == 0))
        .flatten();
    match edge {
        Some(edge) => start(hwnd, edge),
        None => undock(hwnd),
    }
}

pub(super) fn tick(hwnd: HWND) {
    let Some(Some(mut dock)) = with_app(|app| {
        // 拖动或右键菜单期间保持展开。
        if app.move_origin.is_some() || app.menu_open {
            if let Some(dock) = &mut app.edge_dock {
                dock.away = 0;
            }
            return None;
        }
        app.edge_dock
    }) else {
        return;
    };
    if !unsafe { IsWindowVisible(hwnd).as_bool() } {
        return;
    }
    let mut cursor = POINT::default();
    let mut rect = RECT::default();
    if unsafe { GetCursorPos(&mut cursor).is_err() || GetWindowRect(hwnd, &mut rect).is_err() } {
        return;
    }
    let inside =
        (rect.left..rect.right).contains(&cursor.x) && (rect.top..rect.bottom).contains(&cursor.y);
    // 先更新状态再改窗口：SetWindowPos 会同步触发 WM_SIZE 与重绘，绘制时要按新状态选择胶囊或完整面板。
    let mut target = None;
    if let Some(remaining) = dock.collapse_in {
        if remaining > 1 {
            dock.collapse_in = Some(remaining - 1);
        } else {
            dock.collapse_in = None;
            if let Some(capsule) = capsule_rect(dock.edge, rect, window_dpi(hwnd)) {
                dock.shown = rect;
                dock.hidden = true;
                // 鼠标若仍停在原处，先等它离开一次再响应悬停展开，避免刚收起又弹开。
                dock.wait_leave = true;
                target = Some(capsule);
            }
        }
    }
    if !inside {
        dock.wait_leave = false;
    }
    if inside && dock.wait_leave {
        // 刚收起且鼠标仍在胶囊上：保持收起，等鼠标离开后再响应悬停。
    } else if inside {
        dock.away = 0;
        if dock.hidden {
            dock.hidden = false;
            target = Some(dock.shown);
        }
    } else if !dock.hidden && dock.collapse_in.is_none() {
        dock.away = dock.away.saturating_add(1);
        if dock.away >= HIDE_AFTER_TICKS
            && let Some(capsule) = capsule_rect(dock.edge, rect, window_dpi(hwnd))
        {
            dock.shown = rect;
            dock.hidden = true;
            target = Some(capsule);
        }
    }
    with_app(|app| {
        if app.edge_dock.is_some() {
            app.edge_dock = Some(dock);
        }
    });
    if let Some(rect) = target {
        place_rect(hwnd, rect);
    }
}

fn is_vertical(edge: Edge) -> bool {
    matches!(edge, Edge::Left | Edge::Right)
}

impl EdgeDock {
    /// 胶囊是否竖直（贴在左右边缘）。
    pub(super) fn vertical(self) -> bool {
        is_vertical(self.edge)
    }

    /// 是否已收起成小胶囊。
    pub(super) fn hidden(self) -> bool {
        self.hidden
    }
}
