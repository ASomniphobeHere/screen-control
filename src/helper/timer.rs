//! The timer: a small dark pill in a corner of the screen with the time left today.
//!
//! - Always on top, not in the taskbar, never takes focus.
//! - Green, yellow under 10 minutes, red under 5 minutes.
//! - Drag it to move it; it snaps to the nearest corner. Click it to switch between large and
//!   small. The corner and size are saved in `timer.json`.
//! - Counts down by itself each second between the service's `status` messages.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM};
use windows::Win32::Graphics::Direct2D::Common::{D2D_RECT_F, D2D1_COLOR_F};
use windows::Win32::Graphics::Gdi::{
    MONITOR_DEFAULTTONEAREST, MONITOR_DEFAULTTOPRIMARY, MonitorFromPoint, MonitorFromWindow,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, GWLP_USERDATA, GetCursorPos, GetSystemMetrics,
    GetWindowLongPtrW, GetWindowRect, HWND_TOPMOST, IDC_ARROW, LoadCursorW, MA_NOACTIVATE,
    PostMessageW, RegisterClassExW, SM_CXDRAG, SM_CYDRAG, SW_HIDE, SW_SHOWNOACTIVATE,
    SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, SetWindowLongPtrW, SetWindowPos,
    ShowWindow, WM_APP, WM_CAPTURECHANGED, WM_CLOSE, WM_DISPLAYCHANGE, WM_DPICHANGED,
    WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEACTIVATE, WM_MOUSEMOVE, WM_NCDESTROY, WM_SETTINGCHANGE,
    WNDCLASSEXW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};
use windows::core::w;
use windows_numerics::Vector2;

use super::draw::{self, Draw, Font, Painter, Surface, Text, colour, rgba};
use crate::storage;

/// Yellow under this many seconds left.
const YELLOW_UNDER_S: i64 = 10 * 60;
/// Red under this many seconds left.
const RED_UNDER_S: i64 = 5 * 60;

/// The service sends `status` every 20 seconds. Between them the timer counts down by itself,
/// but not for longer than this, in case the service stopped counting.
const GUESS_MAX: Duration = Duration::from_secs(40);
/// With no `status` for this long the service is gone, so the timer hides.
const STALE: Duration = Duration::from_secs(90);

/// Space between the pill and the edges of the screen, in DIPs.
const EDGE: f32 = 16.0;

/// Sizes of the large pill, in DIPs, from the design.
mod large {
    pub const HEIGHT: f32 = 64.0;
    pub const PAD_LEFT: f32 = 14.0;
    pub const PAD_RIGHT: f32 = 22.0;
    pub const RING: f32 = 40.0;
    pub const RING_RADIUS: f32 = 16.0;
    pub const RING_WIDTH: f32 = 4.0;
    pub const GAP: f32 = 14.0;
    pub const TIME_SIZE: f32 = 26.0;
    pub const LABEL_SIZE: f32 = 12.0;
    pub const LINE_HEIGHT: f32 = 1.1;
    pub const SHADOW_DY: f32 = 6.0;
    pub const SHADOW_BLUR: f32 = 20.0;
    pub const SHADOW_ALPHA: f32 = 0.35;
}

/// Sizes of the small pill, in DIPs.
mod small {
    pub const HEIGHT: f32 = 34.0;
    pub const PAD_LEFT: f32 = 10.0;
    pub const PAD_RIGHT: f32 = 14.0;
    pub const DOT: f32 = 10.0;
    pub const GAP: f32 = 8.0;
    pub const TIME_SIZE: f32 = 15.0;
    pub const SHADOW_DY: f32 = 4.0;
    pub const SHADOW_BLUR: f32 = 12.0;
    pub const SHADOW_ALPHA: f32 = 0.3;
}

/// Empty space around the pill for the shadow, in DIPs.
const MARGIN: f32 = large::SHADOW_BLUR + large::SHADOW_DY;

/// Posted to the window to place and draw it again, after the screen or DPI changes.
const WM_APP_LAYOUT: u32 = WM_APP + 1;

// ---- Pure logic ----

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Green,
    Yellow,
    Red,
}

pub fn level(left_s: i64) -> Level {
    if left_s < RED_UNDER_S {
        Level::Red
    } else if left_s < YELLOW_UNDER_S {
        Level::Yellow
    } else {
        Level::Green
    }
}

/// The colour of the ring and dot, and of the large time text.
fn colours(level: Level) -> (D2D1_COLOR_F, D2D1_COLOR_F) {
    match level {
        Level::Green => (colour::GREEN, colour::TEXT),
        Level::Yellow => (colour::YELLOW, colour::TEXT_YELLOW),
        Level::Red => (colour::RED, colour::TEXT_RED),
    }
}

/// The time left as text: `1:25` (hours and minutes) from one hour, `8 min` under an hour, and
/// `0:45` (minutes and seconds) under a minute. Minutes are rounded up, so `1 min` shows until
/// only seconds are left.
pub fn format_left(left_s: i64) -> String {
    let left_s = left_s.max(0);
    if left_s < 60 {
        return format!("0:{left_s:02}");
    }
    let minutes = (left_s + 59) / 60;
    if minutes < 60 {
        format!("{minutes} min")
    } else {
        format!("{}:{:02}", minutes / 60, minutes % 60)
    }
}

/// How much of the ring is coloured: the time left as a part of today's limit, from 0 to 1.
/// With no limit (0), the ring is full while there is time.
pub fn ring_part(left_s: i64, limit_s: u64) -> f32 {
    if left_s <= 0 {
        0.0
    } else if limit_s == 0 {
        1.0
    } else {
        (left_s as f64 / limit_s as f64).clamp(0.0, 1.0) as f32
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Corner {
    TopLeft,
    TopRight,
    BottomLeft,
    #[default]
    BottomRight,
}

/// The corner of `area` nearest to `point`.
pub fn nearest_corner(point: POINT, area: RECT) -> Corner {
    let left = point.x * 2 < area.left + area.right;
    let top = point.y * 2 < area.top + area.bottom;
    match (top, left) {
        (true, true) => Corner::TopLeft,
        (true, false) => Corner::TopRight,
        (false, true) => Corner::BottomLeft,
        (false, false) => Corner::BottomRight,
    }
}

/// The top left of a box of `size` in `corner` of `area`, `edge` pixels from the sides.
pub fn corner_pos(corner: Corner, area: RECT, size: SIZE, edge: i32) -> POINT {
    let left = area.left + edge;
    let right = area.right - edge - size.cx;
    let top = area.top + edge;
    let bottom = area.bottom - edge - size.cy;
    match corner {
        Corner::TopLeft => POINT { x: left, y: top },
        Corner::TopRight => POINT { x: right, y: top },
        Corner::BottomLeft => POINT { x: left, y: bottom },
        Corner::BottomRight => POINT {
            x: right,
            y: bottom,
        },
    }
}

/// Where the child put the timer. Saved in `%LOCALAPPDATA%\ScreenControl\timer.json`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Prefs {
    pub corner: Corner,
    pub small: bool,
}

impl Prefs {
    /// A missing or broken file gives the defaults. It is only a preference.
    pub fn load(path: &Path) -> Prefs {
        match storage::load_json(path) {
            Ok(prefs) => prefs.unwrap_or_default(),
            Err(e) => {
                tracing::warn!(error = format!("{e:#}"), "bad timer.json, using defaults");
                Prefs::default()
            }
        }
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)
                .with_context(|| format!("cannot make {}", dir.display()))?;
        }
        storage::save_json(path, self)
    }
}

/// The last `status` and when it came.
#[derive(Debug, Clone, Copy)]
pub struct Countdown {
    pub left_s: i64,
    pub limit_s: u64,
    pub at: Instant,
}

impl Countdown {
    /// Seconds left at `now`, counted down from the last status. `None` if the status is too
    /// old to show.
    pub fn left_at(&self, now: Instant) -> Option<i64> {
        let since = now.saturating_duration_since(self.at);
        if since >= STALE {
            return None;
        }
        Some(self.left_s - since.min(GUESS_MAX).as_secs() as i64)
    }
}

// ---- The window ----

/// The timer window. Lives on the helper's window thread.
pub struct Timer {
    state: Rc<RefCell<State>>,
}

struct State {
    hwnd: HWND,
    draw: Rc<Draw>,
    surface: Surface,
    prefs: Prefs,
    prefs_path: Option<PathBuf>,
    countdown: Option<Countdown>,
    show_timer: bool,
    visible: bool,
    press: Option<Press>,
}

/// The mouse button is down on the timer.
struct Press {
    /// Cursor and window positions when the button went down.
    cursor: POINT,
    window: POINT,
    dragging: bool,
}

impl Timer {
    /// Makes the window, hidden until the first `status`.
    pub fn new(draw: Rc<Draw>, prefs_path: Option<PathBuf>) -> Result<Timer> {
        let class = w!("ScreenControlTimer");
        let instance = unsafe { GetModuleHandleW(None) }?;
        let wc = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(wndproc),
            hInstance: instance.into(),
            hCursor: unsafe { LoadCursorW(None, IDC_ARROW) }?,
            lpszClassName: class,
            ..Default::default()
        };
        if unsafe { RegisterClassExW(&wc) } == 0 {
            return Err(windows::core::Error::from_thread()).context("cannot register the timer");
        }
        let hwnd = unsafe {
            CreateWindowExW(
                WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_LAYERED,
                class,
                w!("Screen time"),
                WS_POPUP,
                0,
                0,
                0,
                0,
                None,
                None,
                Some(instance.into()),
                None,
            )
        }
        .context("cannot make the timer window")?;
        let prefs = prefs_path.as_deref().map(Prefs::load).unwrap_or_default();
        let state = Rc::new(RefCell::new(State {
            hwnd,
            draw,
            surface: Surface::new(),
            prefs,
            prefs_path,
            countdown: None,
            show_timer: false,
            visible: false,
            press: None,
        }));
        // The window keeps its own reference, freed at WM_NCDESTROY.
        let raw = Rc::into_raw(Rc::clone(&state));
        unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, raw as isize) };
        Ok(Timer { state })
    }

    pub fn set_status(&self, left_s: i64, limit_s: u64, show_timer: bool) {
        let mut state = self.state.borrow_mut();
        state.countdown = Some(Countdown {
            left_s,
            limit_s,
            at: Instant::now(),
        });
        state.show_timer = show_timer;
        state.redraw();
    }

    /// At a warning the timer goes back to large size. It is not saved: the child's choice
    /// comes back when the helper starts again.
    pub fn make_large(&self) {
        let mut state = self.state.borrow_mut();
        if state.prefs.small {
            state.prefs.small = false;
            state.redraw();
        }
    }

    /// Called every second.
    pub fn tick(&self) {
        let mut state = self.state.borrow_mut();
        state.redraw();
        if state.visible {
            // Other topmost windows may have come above it.
            let flags = SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE;
            let _ = unsafe { SetWindowPos(state.hwnd, Some(HWND_TOPMOST), 0, 0, 0, 0, flags) };
        }
    }
}

impl State {
    fn redraw(&mut self) {
        if let Err(e) = self.frame() {
            tracing::warn!(error = format!("{e:#}"), "cannot draw the timer");
        }
    }

    fn frame(&mut self) -> Result<()> {
        let left = self
            .countdown
            .and_then(|c| Some((c.left_at(Instant::now())?, c.limit_s)));
        let Some((left_s, limit_s)) = left.filter(|_| self.show_timer) else {
            if self.visible {
                let _ = unsafe { ShowWindow(self.hwnd, SW_HIDE) };
                self.visible = false;
            }
            return Ok(());
        };
        let monitor = unsafe { MonitorFromWindow(self.hwnd, MONITOR_DEFAULTTOPRIMARY) };
        let (area, dpi) = draw::monitor_area(monitor);
        let scale = dpi as f32 / 96.0;

        let pill = Pill::new(&self.draw, left_s, limit_s, self.prefs.small)?;
        let px = |dips: f32| (dips * scale).round() as i32;
        let size = SIZE {
            cx: ((pill.width + 2.0 * MARGIN) * scale).ceil() as i32,
            cy: ((pill.height + 2.0 * MARGIN) * scale).ceil() as i32,
        };
        let pos = if self.press.as_ref().is_some_and(|p| p.dragging) {
            None
        } else {
            let pill_size = SIZE {
                cx: px(pill.width),
                cy: px(pill.height),
            };
            let p = corner_pos(self.prefs.corner, area, pill_size, px(EDGE));
            Some(POINT {
                x: p.x - px(MARGIN),
                y: p.y - px(MARGIN),
            })
        };
        let draw = Rc::clone(&self.draw);
        self.surface
            .show(&draw, self.hwnd, pos, size, dpi, |p| pill.paint(p))?;
        if !self.visible {
            let _ = unsafe { ShowWindow(self.hwnd, SW_SHOWNOACTIVATE) };
            self.visible = true;
        }
        Ok(())
    }

    /// Handles a mouse or layout message. Returns false to let Windows handle it.
    fn on_message(&mut self, msg: u32) -> bool {
        match msg {
            WM_LBUTTONDOWN => {
                let mut window = RECT::default();
                let _ = unsafe { GetWindowRect(self.hwnd, &mut window) };
                self.press = Some(Press {
                    cursor: cursor(),
                    window: POINT {
                        x: window.left,
                        y: window.top,
                    },
                    dragging: false,
                });
                unsafe { SetCapture(self.hwnd) };
            }
            WM_MOUSEMOVE => {
                let Some(press) = &mut self.press else {
                    return false;
                };
                let now = cursor();
                let (dx, dy) = (now.x - press.cursor.x, now.y - press.cursor.y);
                let far = dx.abs() > unsafe { GetSystemMetrics(SM_CXDRAG) }
                    || dy.abs() > unsafe { GetSystemMetrics(SM_CYDRAG) };
                press.dragging |= far;
                if press.dragging {
                    let flags = SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE;
                    let (x, y) = (press.window.x + dx, press.window.y + dy);
                    let _ = unsafe { SetWindowPos(self.hwnd, None, x, y, 0, 0, flags) };
                }
            }
            WM_LBUTTONUP => {
                let Some(press) = self.press.take() else {
                    return false;
                };
                // Sends WM_CAPTURECHANGED, which finds `press` already gone.
                let _ = unsafe { ReleaseCapture() };
                self.end_press(press);
            }
            WM_CAPTURECHANGED => {
                // Capture lost before the button went up: keep a drag, but no click.
                if let Some(press) = self.press.take()
                    && press.dragging
                {
                    self.end_press(press);
                }
            }
            WM_APP_LAYOUT => {
                if self.press.is_none() {
                    self.redraw();
                }
            }
            _ => return false,
        }
        true
    }

    /// A drag snaps to the nearest corner of the screen it ends on. A click switches the size.
    fn end_press(&mut self, press: Press) {
        if press.dragging {
            let mut window = RECT::default();
            let _ = unsafe { GetWindowRect(self.hwnd, &mut window) };
            let centre = POINT {
                x: (window.left + window.right) / 2,
                y: (window.top + window.bottom) / 2,
            };
            let monitor = unsafe { MonitorFromPoint(centre, MONITOR_DEFAULTTONEAREST) };
            let (area, _) = draw::monitor_area(monitor);
            self.prefs.corner = nearest_corner(centre, area);
        } else {
            self.prefs.small = !self.prefs.small;
        }
        if let Some(path) = &self.prefs_path
            && let Err(e) = self.prefs.save(path)
        {
            tracing::warn!(error = format!("{e:#}"), "cannot save timer.json");
        }
        tracing::info!(prefs = ?self.prefs, "timer moved or resized");
        self.redraw();
    }
}

fn cursor() -> POINT {
    let mut p = POINT::default();
    let _ = unsafe { GetCursorPos(&mut p) };
    p
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let state = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *const RefCell<State>;
    match msg {
        WM_MOUSEACTIVATE => return LRESULT(MA_NOACTIVATE as isize),
        // It cannot be closed.
        WM_CLOSE => return LRESULT(0),
        // Placed again later, when the state is free. The suggested rectangle is not used:
        // the timer places itself in a corner.
        WM_DPICHANGED | WM_DISPLAYCHANGE | WM_SETTINGCHANGE => {
            let _ = unsafe { PostMessageW(Some(hwnd), WM_APP_LAYOUT, WPARAM(0), LPARAM(0)) };
            if msg == WM_DPICHANGED {
                return LRESULT(0);
            }
        }
        WM_NCDESTROY if !state.is_null() => {
            unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0) };
            drop(unsafe { Rc::from_raw(state) });
        }
        WM_LBUTTONDOWN | WM_MOUSEMOVE | WM_LBUTTONUP | WM_CAPTURECHANGED | WM_APP_LAYOUT
            if !state.is_null() =>
        {
            // Busy when the message comes from inside one of our own calls; then Windows
            // handles it.
            if let Ok(mut state) = unsafe { &*state }.try_borrow_mut()
                && state.on_message(msg)
            {
                return LRESULT(0);
            }
        }
        _ => {}
    }
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

/// The pill for one frame: its texts and size in DIPs.
struct Pill {
    small: bool,
    level: Level,
    part: f32,
    time: Text,
    label: Option<Text>,
    width: f32,
    height: f32,
}

impl Pill {
    fn new(draw: &Draw, left_s: i64, limit_s: u64, small: bool) -> Result<Pill> {
        let text = format_left(left_s);
        let level = level(left_s);
        let part = ring_part(left_s, limit_s);
        if small {
            let time = draw.text(
                &text,
                &Font {
                    size: small::TIME_SIZE,
                    weight: draw::SEMI_BOLD,
                    tabular: true,
                    line_height: None,
                    spacing: 0.0,
                },
            )?;
            let width = small::PAD_LEFT + small::DOT + small::GAP + time.width + small::PAD_RIGHT;
            return Ok(Pill {
                small,
                level,
                part,
                time,
                label: None,
                width,
                height: small::HEIGHT,
            });
        }
        let time = draw.text(
            &text,
            &Font {
                size: large::TIME_SIZE,
                weight: draw::SEMI_BOLD,
                tabular: true,
                line_height: Some(large::LINE_HEIGHT),
                spacing: 0.0,
            },
        )?;
        let label = draw.text(
            "left today",
            &Font {
                size: large::LABEL_SIZE,
                weight: draw::NORMAL,
                tabular: false,
                line_height: Some(large::LINE_HEIGHT),
                spacing: 0.0,
            },
        )?;
        let column = time.width.max(label.width);
        let width = large::PAD_LEFT + large::RING + large::GAP + column + large::PAD_RIGHT;
        Ok(Pill {
            small,
            level,
            part,
            time,
            label: Some(label),
            width,
            height: large::HEIGHT,
        })
    }

    /// Draws the pill with its top left at (`MARGIN`, `MARGIN`).
    fn paint(&self, p: &Painter) -> Result<()> {
        let (ring_colour, time_colour) = colours(self.level);
        let (x, y) = (MARGIN, MARGIN);
        let rect = D2D_RECT_F {
            left: x,
            top: y,
            right: x + self.width,
            bottom: y + self.height,
        };
        let radius = self.height / 2.0;
        let middle = y + self.height / 2.0;
        if self.small {
            p.shadow(
                rect,
                radius,
                small::SHADOW_DY,
                small::SHADOW_BLUR,
                rgba(0, small::SHADOW_ALPHA),
            )?;
            p.round_rect(rect, radius, colour::PILL)?;
            let dot = Vector2 {
                X: x + small::PAD_LEFT + small::DOT / 2.0,
                Y: middle,
            };
            p.circle(dot, small::DOT / 2.0, ring_colour)?;
            let tx = x + small::PAD_LEFT + small::DOT + small::GAP;
            p.text(
                &self.time,
                tx,
                middle - self.time.height / 2.0,
                colour::TEXT,
            )?;
            return Ok(());
        }
        p.shadow(
            rect,
            radius,
            large::SHADOW_DY,
            large::SHADOW_BLUR,
            rgba(0, large::SHADOW_ALPHA),
        )?;
        p.round_rect(rect, radius, colour::PILL)?;
        let centre = Vector2 {
            X: x + large::PAD_LEFT + large::RING / 2.0,
            Y: middle,
        };
        p.ring(
            centre,
            large::RING_RADIUS,
            large::RING_WIDTH,
            1.0,
            colour::TRACK,
        )?;
        p.ring(
            centre,
            large::RING_RADIUS,
            large::RING_WIDTH,
            self.part,
            ring_colour,
        )?;
        let tx = x + large::PAD_LEFT + large::RING + large::GAP;
        let label_height = self.label.as_ref().map_or(0.0, |l| l.height);
        let ty = middle - (self.time.height + label_height) / 2.0;
        p.text(&self.time, tx, ty, time_colour)?;
        if let Some(label) = &self.label {
            p.text(label, tx, ty + self.time.height, colour::LABEL)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colour_changes_under_10_and_5_minutes() {
        assert_eq!(level(3 * 3600), Level::Green);
        assert_eq!(level(600), Level::Green);
        assert_eq!(level(599), Level::Yellow);
        assert_eq!(level(300), Level::Yellow);
        assert_eq!(level(299), Level::Red);
        assert_eq!(level(0), Level::Red);
        assert_eq!(level(-30), Level::Red);
    }

    #[test]
    fn time_text() {
        assert_eq!(format_left(5100), "1:25");
        assert_eq!(format_left(3600), "1:00");
        // 59 min 59 s rounds up to a full hour.
        assert_eq!(format_left(3599), "1:00");
        assert_eq!(format_left(3540), "59 min");
        assert_eq!(format_left(8 * 60), "8 min");
        assert_eq!(format_left(7 * 60 + 1), "8 min");
        assert_eq!(format_left(60), "1 min");
        assert_eq!(format_left(59), "0:59");
        assert_eq!(format_left(5), "0:05");
        assert_eq!(format_left(0), "0:00");
        assert_eq!(format_left(-20), "0:00");
        assert_eq!(format_left(10 * 3600), "10:00");
    }

    #[test]
    fn ring_is_part_of_the_limit() {
        assert_eq!(ring_part(3600, 7200), 0.5);
        assert_eq!(ring_part(7200, 7200), 1.0);
        assert_eq!(ring_part(9000, 7200), 1.0);
        assert_eq!(ring_part(0, 7200), 0.0);
        assert_eq!(ring_part(-5, 7200), 0.0);
        assert_eq!(ring_part(100, 0), 1.0);
        assert_eq!(ring_part(0, 0), 0.0);
    }

    const AREA: RECT = RECT {
        left: 0,
        top: 0,
        right: 1920,
        bottom: 1040,
    };

    #[test]
    fn snaps_to_the_nearest_corner() {
        let at = |x, y| nearest_corner(POINT { x, y }, AREA);
        assert_eq!(at(10, 10), Corner::TopLeft);
        assert_eq!(at(1900, 30), Corner::TopRight);
        assert_eq!(at(100, 1000), Corner::BottomLeft);
        assert_eq!(at(1500, 700), Corner::BottomRight);
        // Off the screen still gives the nearest corner.
        assert_eq!(at(-50, 2000), Corner::BottomLeft);
    }

    #[test]
    fn nearest_corner_on_a_second_screen() {
        let area = RECT {
            left: 1920,
            top: -200,
            right: 3200,
            bottom: 824,
        };
        assert_eq!(
            nearest_corner(POINT { x: 2000, y: -100 }, area),
            Corner::TopLeft
        );
        assert_eq!(
            nearest_corner(POINT { x: 3000, y: 700 }, area),
            Corner::BottomRight
        );
    }

    #[test]
    fn corner_positions_keep_the_edge() {
        let size = SIZE { cx: 200, cy: 64 };
        let pos = |c| corner_pos(c, AREA, size, 16);
        assert_eq!(pos(Corner::TopLeft), POINT { x: 16, y: 16 });
        assert_eq!(pos(Corner::TopRight), POINT { x: 1704, y: 16 });
        assert_eq!(pos(Corner::BottomLeft), POINT { x: 16, y: 960 });
        assert_eq!(pos(Corner::BottomRight), POINT { x: 1704, y: 960 });
    }

    #[test]
    fn countdown_ticks_down_between_statuses() {
        let at = Instant::now();
        let c = Countdown {
            left_s: 600,
            limit_s: 3600,
            at,
        };
        assert_eq!(c.left_at(at), Some(600));
        assert_eq!(c.left_at(at + Duration::from_millis(999)), Some(600));
        assert_eq!(c.left_at(at + Duration::from_secs(15)), Some(585));
        // Stops guessing when the next status is late.
        assert_eq!(c.left_at(at + Duration::from_secs(60)), Some(560));
        // Hidden when the service is gone.
        assert_eq!(c.left_at(at + STALE), None);
    }

    fn test_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "screen-control-test-timer-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn prefs_save_and_load() {
        let path = test_dir("prefs").join("ScreenControl").join("timer.json");
        assert_eq!(Prefs::load(&path), Prefs::default());
        let prefs = Prefs {
            corner: Corner::TopLeft,
            small: true,
        };
        prefs.save(&path).unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "{\n  \"corner\": \"top_left\",\n  \"small\": true\n}"
        );
        assert_eq!(Prefs::load(&path), prefs);
    }

    #[test]
    fn broken_or_partial_prefs_use_defaults() {
        let dir = test_dir("broken");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("timer.json");
        std::fs::write(&path, "not json").unwrap();
        assert_eq!(Prefs::load(&path), Prefs::default());
        std::fs::write(&path, r#"{"small":true}"#).unwrap();
        assert_eq!(
            Prefs::load(&path),
            Prefs {
                corner: Corner::BottomRight,
                small: true
            }
        );
    }
}
