//! The popups: a warning at 10, 5 and 1 minute, "time is up" with a countdown to the sign-out,
//! and a small note when a parent gives or takes time.
//!
//! - Only one popup at a time. A newer one replaces an older one.
//! - The warning and time-up cards show in the middle of the screen and come to the front. The
//!   note shows at the top of the screen and does not take focus.
//! - The warning closes after 15 seconds, with its OK button, or with Enter, Esc or Space.
//! - Time-up cannot be closed. The sign-out ends it, or a `status` with time left (a parent gave
//!   time during the countdown).
//! - The note closes after 10 seconds or with a click.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM};
use windows::Win32::Graphics::Direct2D::Common::{D2D_RECT_F, D2D1_COLOR_F};
use windows::Win32::Graphics::DirectWrite::DWRITE_FONT_WEIGHT;
use windows::Win32::Graphics::Gdi::{
    HMONITOR, MONITOR_DEFAULTTOPRIMARY, MonitorFromWindow, ScreenToClient,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
use windows::Win32::UI::Input::KeyboardAndMouse::{VK_ESCAPE, VK_RETURN, VK_SPACE};
use windows::Win32::UI::WindowsAndMessaging::{
    BringWindowToTop, CreateWindowExW, DefWindowProcW, GWLP_USERDATA, GetCursorPos,
    GetForegroundWindow, GetWindowLongPtrW, GetWindowThreadProcessId, HWND_TOPMOST, IDC_ARROW,
    IDC_HAND, KillTimer, LoadCursorW, MA_NOACTIVATE, PostMessageW, RegisterClassExW, SW_HIDE,
    SW_SHOW, SW_SHOWNOACTIVATE, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SetCursor,
    SetForegroundWindow, SetTimer, SetWindowLongPtrW, SetWindowPos, ShowWindow, WM_APP, WM_CLOSE,
    WM_DISPLAYCHANGE, WM_DPICHANGED, WM_KEYDOWN, WM_LBUTTONUP, WM_MOUSEACTIVATE, WM_NCDESTROY,
    WM_SETCURSOR, WM_SETTINGCHANGE, WM_TIMER, WNDCLASSEXW, WS_EX_LAYERED, WS_EX_TOOLWINDOW,
    WS_EX_TOPMOST, WS_POPUP,
};
use windows::core::w;
use windows_numerics::Vector2;

use super::draw::{self, Draw, Font, Painter, Surface, Text, colour, rgba};
use super::timer::{Level, level};
use crate::core::engine::Reason;

/// The warning closes by itself after this time.
const WARN_CLOSE: Duration = Duration::from_secs(15);
/// The note about extra time closes by itself after this time.
const NOTE_CLOSE: Duration = Duration::from_secs(10);
/// Keys and clicks right after a popup shows are not used, so a child who is typing or
/// clicking does not close it before reading it.
const INPUT_DELAY: Duration = Duration::from_millis(800);

/// The popup is drawn again this often, for the countdown and to close in time.
const FRAME_MS: u32 = 200;
const FRAME_TIMER: usize = 1;

/// Posted to the window to place and draw it again, after the screen or DPI changes.
const WM_APP_LAYOUT: u32 = WM_APP + 1;

/// Sizes of the warning and time-up cards, in DIPs, from the design.
mod card {
    pub const WIDTH: f32 = 400.0;
    pub const PAD_X: f32 = 32.0;
    pub const PAD_Y: f32 = 28.0;
    pub const RADIUS: f32 = 20.0;
    pub const GAP: f32 = 16.0;
    pub const LABEL_SIZE: f32 = 12.0;
    pub const LABEL_SPACING: f32 = 0.08;
    pub const NUMBER_SIZE: f32 = 72.0;
    pub const UNIT_SIZE: f32 = 22.0;
    pub const NUMBER_GAP: f32 = 12.0;
    pub const BODY_SIZE: f32 = 16.0;
    pub const BODY_LINE: f32 = 1.45;
    pub const BUTTON_WIDTH: f32 = 96.0;
    pub const BUTTON_HEIGHT: f32 = 44.0;
    pub const BUTTON_TEXT: f32 = 15.0;
    pub const TITLE_SIZE: f32 = 34.0;
    pub const BAR_HEIGHT: f32 = 6.0;
    pub const HINT_SIZE: f32 = 14.0;
    pub const SHADOW_DY: f32 = 16.0;
    pub const SHADOW_BLUR: f32 = 48.0;
    pub const SHADOW_ALPHA: f32 = 0.45;
}

/// Sizes of the note about extra time, in DIPs, from the design.
mod note {
    pub const WIDTH: f32 = 300.0;
    pub const PAD_X: f32 = 20.0;
    pub const PAD_Y: f32 = 16.0;
    pub const RADIUS: f32 = 18.0;
    pub const GAP: f32 = 14.0;
    pub const ICON: f32 = 36.0;
    pub const ICON_RADIUS: f32 = 17.0;
    /// Half the length of the lines of the plus or minus sign.
    pub const SIGN: f32 = 7.0;
    pub const SIGN_WIDTH: f32 = 2.5;
    pub const TITLE_SIZE: f32 = 16.0;
    pub const TEXT_SIZE: f32 = 13.0;
    pub const TEXT_GAP: f32 = 2.0;
    pub const SHADOW_DY: f32 = 12.0;
    pub const SHADOW_BLUR: f32 = 32.0;
    pub const SHADOW_ALPHA: f32 = 0.4;
    /// Space between the note and the top of the screen.
    pub const EDGE: f32 = 24.0;
}

// ---- Pure logic ----

/// What a popup shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Content {
    Warn {
        name: Option<String>,
        minutes_left: u32,
        reason: Reason,
    },
    TimeUp {
        name: Option<String>,
        grace_s: u64,
        reason: Reason,
        can_ask: bool,
    },
    ExtraTime {
        minutes: i32,
        by: String,
    },
}

impl Content {
    /// When the popup closes by itself. `None`: it does not.
    pub fn closes_after(&self) -> Option<Duration> {
        match self {
            Content::Warn { .. } => Some(WARN_CLOSE),
            Content::TimeUp { .. } => None,
            Content::ExtraTime { .. } => Some(NOTE_CLOSE),
        }
    }

    /// The small note at the top, which does not take focus.
    pub fn is_note(&self) -> bool {
        matches!(self, Content::ExtraTime { .. })
    }

    /// The child can close it.
    pub fn can_close(&self) -> bool {
        !matches!(self, Content::TimeUp { .. })
    }

    /// A `status` with `left_s` ends the popup: time-up ends when time was given.
    pub fn ends_with_status(&self, left_s: i64) -> bool {
        matches!(self, Content::TimeUp { .. }) && left_s > 0
    }
}

/// `text` (which starts with a small letter) for the child: with the name in front, or with a
/// capital letter.
pub fn address(name: Option<&str>, text: &str) -> String {
    match name.filter(|n| !n.trim().is_empty()) {
        Some(name) => format!("{name}, {text}"),
        None => {
            let mut chars = text.chars();
            chars
                .next()
                .map_or(String::new(), |c| c.to_uppercase().chain(chars).collect())
        }
    }
}

/// The words after the large number of a warning.
pub fn minutes_left_text(minutes: u32) -> &'static str {
    if minutes == 1 {
        "minute left"
    } else {
        "minutes left"
    }
}

/// The colour level of a warning's number, the same as the timer shows at that time.
pub fn warn_level(minutes: u32) -> Level {
    // The warning comes when the time left is at or just under the minutes.
    level(minutes as i64 * 60 - 1)
}

pub fn warn_text(name: Option<&str>, reason: Reason) -> String {
    let why = match reason {
        Reason::Budget => "your screen time for today ends soon.",
        Reason::Hours => "the allowed hours end soon.",
        Reason::Extra => "your extra time ends soon.",
    };
    format!("{} Save what you are doing.", address(name, why))
}

/// The small text under the time-up countdown.
pub fn time_up_text(name: Option<&str>, reason: Reason, can_ask: bool) -> String {
    if can_ask {
        return "To ask for more time, send /more 30 to the bot in Telegram.".into();
    }
    let why = match reason {
        Reason::Budget => "your screen time for today is used up.",
        Reason::Hours => "screen time is not allowed at this hour.",
        Reason::Extra => "your extra time is over.",
    };
    address(name, why)
}

/// Whole seconds until the sign-out, `since` the time-up message came.
pub fn seconds_left(grace_s: u64, since: Duration) -> u64 {
    grace_s.saturating_sub(since.as_secs())
}

/// How much of the countdown bar is coloured, from 1 (full) down to 0.
pub fn bar_part(grace_s: u64, since: Duration) -> f32 {
    if grace_s == 0 {
        return 0.0;
    }
    (1.0 - since.as_secs_f32() / grace_s as f32).clamp(0.0, 1.0)
}

/// Seconds as `0:08`.
pub fn countdown_text(seconds: u64) -> String {
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

/// The two lines of the note: `+30 minutes`, `Mom gave you more time`.
pub fn extra_texts(minutes: i32, by: &str) -> (String, String) {
    let n = minutes.unsigned_abs();
    let unit = if n == 1 { "minute" } else { "minutes" };
    if minutes >= 0 {
        (format!("+{n} {unit}"), format!("{by} gave you more time"))
    } else {
        // A real minus sign, as wide as the plus.
        (
            format!("\u{2212}{n} {unit}"),
            format!("{by} took some of your time"),
        )
    }
}

/// The top left of a box of `size` in the middle of `area`.
pub fn centre_pos(area: RECT, size: SIZE) -> POINT {
    POINT {
        x: area.left + (area.right - area.left - size.cx) / 2,
        y: area.top + (area.bottom - area.top - size.cy) / 2,
    }
}

/// The top left of a box of `size` at the top middle of `area`, `edge` pixels down.
pub fn top_centre_pos(area: RECT, size: SIZE, edge: i32) -> POINT {
    POINT {
        x: area.left + (area.right - area.left - size.cx) / 2,
        y: area.top + edge,
    }
}

/// Places blocks of `heights` under each other with `gap` between them, like a CSS flex column.
/// Gives the top of each block and the full height.
pub fn stack(heights: &[f32], gap: f32) -> (Vec<f32>, f32) {
    let mut tops = Vec::with_capacity(heights.len());
    let mut y = 0.0;
    for (i, h) in heights.iter().enumerate() {
        if i > 0 {
            y += gap;
        }
        tops.push(y);
        y += h;
    }
    (tops, y)
}

pub fn inside(rect: &D2D_RECT_F, x: f32, y: f32) -> bool {
    x >= rect.left && x < rect.right && y >= rect.top && y < rect.bottom
}

/// Keys and clicks close the popup only after `INPUT_DELAY`.
pub fn takes_input(since: Duration) -> bool {
    since >= INPUT_DELAY
}

// ---- The picture ----

/// One thing to draw, in DIPs from the top left of the card.
enum Shape {
    Rect {
        rect: D2D_RECT_F,
        radius: f32,
        colour: D2D1_COLOR_F,
    },
    Circle {
        centre: Vector2,
        radius: f32,
        colour: D2D1_COLOR_F,
    },
    Line {
        from: Vector2,
        to: Vector2,
        width: f32,
        colour: D2D1_COLOR_F,
    },
    Text {
        text: Text,
        x: f32,
        y: f32,
        colour: D2D1_COLOR_F,
    },
}

/// A popup for one frame.
struct Picture {
    width: f32,
    height: f32,
    radius: f32,
    shadow_dy: f32,
    shadow_blur: f32,
    shadow_alpha: f32,
    shapes: Vec<Shape>,
    /// The OK button, in DIPs from the top left of the card.
    button: Option<D2D_RECT_F>,
}

fn font(size: f32, weight: DWRITE_FONT_WEIGHT) -> Font {
    Font {
        size,
        weight,
        tabular: false,
        line_height: None,
        spacing: 0.0,
    }
}

fn rect(x: f32, y: f32, width: f32, height: f32) -> D2D_RECT_F {
    D2D_RECT_F {
        left: x,
        top: y,
        right: x + width,
        bottom: y + height,
    }
}

impl Picture {
    fn new(draw: &Draw, content: &Content, since: Duration) -> Result<Picture> {
        match content {
            Content::Warn {
                name,
                minutes_left,
                reason,
            } => Picture::warn(draw, name.as_deref(), *minutes_left, *reason),
            Content::TimeUp {
                name,
                grace_s,
                reason,
                can_ask,
            } => Picture::time_up(draw, name.as_deref(), *grace_s, *reason, *can_ask, since),
            Content::ExtraTime { minutes, by } => Picture::note(draw, *minutes, by),
        }
    }

    fn card(height: f32, shapes: Vec<Shape>, button: Option<D2D_RECT_F>) -> Picture {
        Picture {
            width: card::WIDTH,
            height,
            radius: card::RADIUS,
            shadow_dy: card::SHADOW_DY,
            shadow_blur: card::SHADOW_BLUR,
            shadow_alpha: card::SHADOW_ALPHA,
            shapes,
            button,
        }
    }

    fn label(draw: &Draw) -> Result<Text> {
        draw.text(
            "SCREEN TIME",
            &Font {
                spacing: card::LABEL_SPACING,
                ..font(card::LABEL_SIZE, draw::SEMI_BOLD)
            },
        )
    }

    fn warn(draw: &Draw, name: Option<&str>, minutes: u32, reason: Reason) -> Result<Picture> {
        let inner = card::WIDTH - 2.0 * card::PAD_X;
        let label = Picture::label(draw)?;
        let number = draw.text(
            &minutes.to_string(),
            &Font {
                tabular: true,
                line_height: Some(1.0),
                ..font(card::NUMBER_SIZE, draw::BOLD)
            },
        )?;
        let unit = draw.text(
            minutes_left_text(minutes),
            &font(card::UNIT_SIZE, draw::SEMI_BOLD),
        )?;
        let body = draw.text_in(
            &warn_text(name, reason),
            &Font {
                line_height: Some(card::BODY_LINE),
                ..font(card::BODY_SIZE, draw::NORMAL)
            },
            inner,
        )?;
        let ok = draw.text("OK", &font(card::BUTTON_TEXT, draw::SEMI_BOLD))?;

        // The number and the words stand on the same base line.
        let above = number.baseline.max(unit.baseline);
        let below = (number.height - number.baseline).max(unit.height - unit.baseline);
        let heights = [
            label.height,
            above + below,
            body.height,
            card::BUTTON_HEIGHT,
        ];
        let (tops, total) = stack(&heights, card::GAP);
        let y = |i: usize| card::PAD_Y + tops[i];
        let x = card::PAD_X;

        let number_colour = match warn_level(minutes) {
            Level::Red => colour::TEXT_RED,
            _ => colour::TEXT_YELLOW,
        };
        let button = rect(
            card::WIDTH - card::PAD_X - card::BUTTON_WIDTH,
            y(3),
            card::BUTTON_WIDTH,
            card::BUTTON_HEIGHT,
        );
        let ok_x = button.left + (card::BUTTON_WIDTH - ok.width) / 2.0;
        let ok_y = button.top + (card::BUTTON_HEIGHT - ok.height) / 2.0;
        let number_y = y(1) + above - number.baseline;
        let unit_y = y(1) + above - unit.baseline;
        let unit_x = x + number.width + card::NUMBER_GAP;
        let shapes = vec![
            Shape::Text {
                text: label,
                x,
                y: y(0),
                colour: colour::MUTED,
            },
            Shape::Text {
                text: number,
                x,
                y: number_y,
                colour: number_colour,
            },
            Shape::Text {
                text: unit,
                x: unit_x,
                y: unit_y,
                colour: colour::TEXT,
            },
            Shape::Text {
                text: body,
                x,
                y: y(2),
                colour: colour::BODY,
            },
            Shape::Rect {
                rect: button,
                radius: card::BUTTON_HEIGHT / 2.0,
                colour: colour::TEXT,
            },
            Shape::Text {
                text: ok,
                x: ok_x,
                y: ok_y,
                colour: colour::CARD,
            },
        ];
        Ok(Picture::card(
            total + 2.0 * card::PAD_Y,
            shapes,
            Some(button),
        ))
    }

    fn time_up(
        draw: &Draw,
        name: Option<&str>,
        grace_s: u64,
        reason: Reason,
        can_ask: bool,
        since: Duration,
    ) -> Result<Picture> {
        let inner = card::WIDTH - 2.0 * card::PAD_X;
        let label = Picture::label(draw)?;
        let title = draw.text("Time is up", &font(card::TITLE_SIZE, draw::BOLD))?;
        let left = seconds_left(grace_s, since);
        let (lead, count) = if left > 0 {
            let count = draw.text(
                &countdown_text(left),
                &Font {
                    tabular: true,
                    ..font(card::BODY_SIZE, draw::BOLD)
                },
            )?;
            ("Signing out in ", Some(count))
        } else {
            ("Signing out now", None)
        };
        let lead = draw.text(lead, &font(card::BODY_SIZE, draw::NORMAL))?;
        let hint = draw.text_in(
            &time_up_text(name, reason, can_ask),
            &Font {
                line_height: Some(card::BODY_LINE),
                ..font(card::HINT_SIZE, draw::NORMAL)
            },
            inner,
        )?;

        let line_height = count
            .as_ref()
            .map_or(lead.height, |c| c.height.max(lead.height));
        let heights = [
            label.height,
            title.height,
            line_height,
            card::BAR_HEIGHT,
            hint.height,
        ];
        let (tops, total) = stack(&heights, card::GAP);
        let y = |i: usize| card::PAD_Y + tops[i];
        let x = card::PAD_X;
        let bar = |part: f32| rect(x, y(3), inner * part, card::BAR_HEIGHT);
        let part = bar_part(grace_s, since);
        let lead_width = lead.width;
        let mut shapes = vec![
            Shape::Text {
                text: label,
                x,
                y: y(0),
                colour: colour::MUTED,
            },
            Shape::Text {
                text: title,
                x,
                y: y(1),
                colour: colour::TEXT,
            },
            Shape::Text {
                text: lead,
                x,
                y: y(2),
                colour: colour::BODY,
            },
            Shape::Rect {
                rect: bar(1.0),
                radius: card::BAR_HEIGHT / 2.0,
                colour: colour::BAR,
            },
            Shape::Text {
                text: hint,
                x,
                y: y(4),
                colour: colour::MUTED,
            },
        ];
        if let Some(count) = count {
            shapes.push(Shape::Text {
                text: count,
                x: x + lead_width,
                y: y(2),
                colour: colour::TEXT_RED,
            });
        }
        // Not narrower than its height, so the round ends stay round.
        if part > 0.0 {
            let mut fill = bar(part);
            fill.right = fill.right.max(fill.left + card::BAR_HEIGHT);
            shapes.push(Shape::Rect {
                rect: fill,
                radius: card::BAR_HEIGHT / 2.0,
                colour: colour::RED,
            });
        }
        Ok(Picture::card(total + 2.0 * card::PAD_Y, shapes, None))
    }

    fn note(draw: &Draw, minutes: i32, by: &str) -> Result<Picture> {
        let (title, text) = extra_texts(minutes, by);
        let text_x = note::PAD_X + note::ICON + note::GAP;
        let text_width = note::WIDTH - text_x - note::PAD_X;
        let title = draw.text_in(&title, &font(note::TITLE_SIZE, draw::SEMI_BOLD), text_width)?;
        let text = draw.text_in(&text, &font(note::TEXT_SIZE, draw::NORMAL), text_width)?;
        let column = title.height + note::TEXT_GAP + text.height;
        let height = 2.0 * note::PAD_Y + column.max(note::ICON);
        let middle = height / 2.0;
        let top = middle - column / 2.0;
        let text_y = top + title.height + note::TEXT_GAP;

        let given = minutes >= 0;
        let (back, sign) = if given {
            (colour::GREEN_BACK, colour::GREEN)
        } else {
            (colour::RED_BACK, colour::RED)
        };
        let centre = Vector2 {
            X: note::PAD_X + note::ICON / 2.0,
            Y: middle,
        };
        let line = |dx: f32, dy: f32| Shape::Line {
            from: Vector2 {
                X: centre.X - dx,
                Y: centre.Y - dy,
            },
            to: Vector2 {
                X: centre.X + dx,
                Y: centre.Y + dy,
            },
            width: note::SIGN_WIDTH,
            colour: sign,
        };
        let mut shapes = vec![
            Shape::Circle {
                centre,
                radius: note::ICON_RADIUS,
                colour: back,
            },
            line(note::SIGN, 0.0),
        ];
        if given {
            shapes.push(line(0.0, note::SIGN));
        }
        shapes.push(Shape::Text {
            text: title,
            x: text_x,
            y: top,
            colour: colour::TEXT,
        });
        shapes.push(Shape::Text {
            text,
            x: text_x,
            y: text_y,
            colour: colour::MUTED,
        });
        Ok(Picture {
            width: note::WIDTH,
            height,
            radius: note::RADIUS,
            shadow_dy: note::SHADOW_DY,
            shadow_blur: note::SHADOW_BLUR,
            shadow_alpha: note::SHADOW_ALPHA,
            shapes,
            button: None,
        })
    }

    /// Empty space around the card for the shadow, in DIPs.
    fn margin(&self) -> f32 {
        self.shadow_blur + self.shadow_dy
    }

    /// Draws the card with its top left at (`margin`, `margin`).
    fn paint(&self, p: &Painter) -> Result<()> {
        let m = self.margin();
        let at = |r: &D2D_RECT_F| D2D_RECT_F {
            left: r.left + m,
            top: r.top + m,
            right: r.right + m,
            bottom: r.bottom + m,
        };
        let point = |v: &Vector2| Vector2 {
            X: v.X + m,
            Y: v.Y + m,
        };
        let card = rect(m, m, self.width, self.height);
        p.shadow(
            card,
            self.radius,
            self.shadow_dy,
            self.shadow_blur,
            rgba(0, self.shadow_alpha),
        )?;
        p.round_rect(card, self.radius, colour::CARD)?;
        for shape in &self.shapes {
            match shape {
                Shape::Rect {
                    rect,
                    radius,
                    colour,
                } => p.round_rect(at(rect), *radius, *colour)?,
                Shape::Circle {
                    centre,
                    radius,
                    colour,
                } => p.circle(point(centre), *radius, *colour)?,
                Shape::Line {
                    from,
                    to,
                    width,
                    colour,
                } => p.line(point(from), point(to), *width, *colour)?,
                Shape::Text { text, x, y, colour } => p.text(text, x + m, y + m, *colour)?,
            }
        }
        Ok(())
    }
}

// ---- The window ----

/// The popup window. One for all popups; it is hidden when no popup shows. Lives on the
/// helper's window thread.
pub struct Popup {
    state: Rc<RefCell<State>>,
}

struct State {
    hwnd: HWND,
    draw: Rc<Draw>,
    surface: Surface,
    shown: Option<Shown>,
    /// The scale and the button of the last frame, to find what the mouse is on.
    scale: f32,
    margin: f32,
    button: Option<D2D_RECT_F>,
}

struct Shown {
    content: Content,
    at: Instant,
}

impl Popup {
    pub fn new(draw: Rc<Draw>) -> Result<Popup> {
        // Not the timer's class: the popups may take focus.
        let class = w!("ScreenControlPopup");
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
            return Err(windows::core::Error::from_thread()).context("cannot register the popup");
        }
        let hwnd = unsafe {
            CreateWindowExW(
                WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_LAYERED,
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
        .context("cannot make the popup window")?;
        let state = Rc::new(RefCell::new(State {
            hwnd,
            draw,
            surface: Surface::new(),
            shown: None,
            scale: 1.0,
            margin: 0.0,
            button: None,
        }));
        // The window keeps its own reference, freed at WM_NCDESTROY.
        let raw = Rc::into_raw(Rc::clone(&state));
        unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, raw as isize) };
        Ok(Popup { state })
    }

    /// Shows `content` in place of any popup that shows now.
    pub fn show(&self, content: Content) {
        let note = content.is_note();
        let hwnd = {
            let mut state = self.state.borrow_mut();
            if let Some(old) = &state.shown {
                tracing::info!(content = ?old.content, "popup replaced");
            }
            tracing::info!(?content, "popup shown");
            state.shown = Some(Shown {
                content,
                at: Instant::now(),
            });
            // On the screen the child uses.
            let monitor =
                unsafe { MonitorFromWindow(GetForegroundWindow(), MONITOR_DEFAULTTOPRIMARY) };
            state.redraw(Some(monitor));
            state.hwnd
        };
        // Outside the borrow: these send messages to the window.
        unsafe {
            SetTimer(Some(hwnd), FRAME_TIMER, FRAME_MS, None);
            if note {
                let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
                let flags = SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE;
                let _ = SetWindowPos(hwnd, Some(HWND_TOPMOST), 0, 0, 0, 0, flags);
            } else {
                let _ = ShowWindow(hwnd, SW_SHOW);
                let flags = SWP_NOMOVE | SWP_NOSIZE;
                let _ = SetWindowPos(hwnd, Some(HWND_TOPMOST), 0, 0, 0, 0, flags);
                bring_to_front(hwnd);
            }
        }
    }

    /// Called for each `status`.
    pub fn on_status(&self, left_s: i64) {
        let mut state = self.state.borrow_mut();
        if state
            .shown
            .as_ref()
            .is_some_and(|s| s.content.ends_with_status(left_s))
        {
            state.close("time was given");
        }
    }
}

/// Windows only lets the program the user works with take the focus. The popup's thread
/// shares input with that program for a moment, which is allowed.
fn bring_to_front(hwnd: HWND) {
    unsafe {
        if SetForegroundWindow(hwnd).as_bool() {
            return;
        }
        tracing::info!("popup not in front, sharing input with the program in front");
        let me = GetCurrentThreadId();
        let other = GetWindowThreadProcessId(GetForegroundWindow(), None);
        if other == 0 || other == me || !AttachThreadInput(me, other, true).as_bool() {
            return;
        }
        let _ = BringWindowToTop(hwnd);
        let ok = SetForegroundWindow(hwnd).as_bool();
        let _ = AttachThreadInput(me, other, false);
        if !ok {
            tracing::info!("cannot bring the popup to the front");
        }
    }
}

impl State {
    /// Draws the popup. `place` puts it on that screen; `None` keeps it where it is.
    fn redraw(&mut self, place: Option<HMONITOR>) {
        if let Err(e) = self.frame(place) {
            tracing::warn!(error = format!("{e:#}"), "cannot draw the popup");
        }
    }

    fn frame(&mut self, place: Option<HMONITOR>) -> Result<()> {
        let Some(shown) = &self.shown else {
            return Ok(());
        };
        let monitor = place
            .unwrap_or_else(|| unsafe { MonitorFromWindow(self.hwnd, MONITOR_DEFAULTTOPRIMARY) });
        let (area, dpi) = draw::monitor_area(monitor);
        let scale = dpi as f32 / 96.0;
        let picture = Picture::new(&self.draw, &shown.content, shown.at.elapsed())?;
        let margin = picture.margin();
        let px = |dips: f32| (dips * scale).round() as i32;
        let size = SIZE {
            cx: ((picture.width + 2.0 * margin) * scale).ceil() as i32,
            cy: ((picture.height + 2.0 * margin) * scale).ceil() as i32,
        };
        let pos = place.map(|_| {
            let card = SIZE {
                cx: px(picture.width),
                cy: px(picture.height),
            };
            let p = if shown.content.is_note() {
                top_centre_pos(area, card, px(note::EDGE))
            } else {
                centre_pos(area, card)
            };
            POINT {
                x: p.x - px(margin),
                y: p.y - px(margin),
            }
        });
        self.scale = scale;
        self.margin = margin;
        self.button = picture.button;
        let draw = Rc::clone(&self.draw);
        self.surface
            .show(&draw, self.hwnd, pos, size, dpi, |p| picture.paint(p))
    }

    fn close(&mut self, why: &str) {
        if let Some(shown) = self.shown.take() {
            tracing::info!(content = ?shown.content, why, "popup closed");
            unsafe {
                let _ = KillTimer(Some(self.hwnd), FRAME_TIMER);
                let _ = ShowWindow(self.hwnd, SW_HIDE);
            }
        }
    }

    /// Closes the popup if the child can close it now.
    fn close_by_child(&mut self) {
        if self
            .shown
            .as_ref()
            .is_some_and(|s| s.content.can_close() && takes_input(s.at.elapsed()))
        {
            self.close("closed by the child");
        }
    }

    /// The point under the mouse, in DIPs from the top left of the card.
    fn mouse(&self, client: POINT) -> (f32, f32) {
        (
            client.x as f32 / self.scale - self.margin,
            client.y as f32 / self.scale - self.margin,
        )
    }

    fn over_button(&self, client: POINT) -> bool {
        let (x, y) = self.mouse(client);
        self.button.as_ref().is_some_and(|b| inside(b, x, y))
    }

    /// Handles a message. Returns false to let Windows handle it.
    fn on_message(&mut self, msg: u32, wparam: WPARAM, lparam: LPARAM) -> bool {
        let Some(shown) = &self.shown else {
            return false;
        };
        match msg {
            WM_TIMER if wparam.0 == FRAME_TIMER => {
                let since = shown.at.elapsed();
                if shown
                    .content
                    .closes_after()
                    .is_some_and(|after| since >= after)
                {
                    self.close("time over");
                } else if matches!(shown.content, Content::TimeUp { .. }) {
                    self.redraw(None);
                }
            }
            WM_LBUTTONUP => {
                let client = POINT {
                    x: (lparam.0 & 0xffff) as i16 as i32,
                    y: ((lparam.0 >> 16) & 0xffff) as i16 as i32,
                };
                if shown.content.is_note() || self.over_button(client) {
                    self.close_by_child();
                }
            }
            WM_KEYDOWN => {
                let key = wparam.0 as u16;
                if [VK_RETURN.0, VK_ESCAPE.0, VK_SPACE.0].contains(&key) {
                    self.close_by_child();
                }
            }
            WM_SETCURSOR => {
                let mut p = POINT::default();
                unsafe {
                    let _ = GetCursorPos(&mut p);
                    let _ = ScreenToClient(self.hwnd, &mut p);
                }
                if !self.over_button(p) {
                    return false;
                }
                if let Ok(hand) = unsafe { LoadCursorW(None, IDC_HAND) } {
                    unsafe { SetCursor(Some(hand)) };
                }
            }
            WM_CLOSE => self.close_by_child(),
            WM_APP_LAYOUT => {
                let monitor = unsafe { MonitorFromWindow(self.hwnd, MONITOR_DEFAULTTOPRIMARY) };
                self.redraw(Some(monitor));
            }
            _ => return false,
        }
        true
    }

    fn is_note(&self) -> bool {
        self.shown.as_ref().is_some_and(|s| s.content.is_note())
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let state = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *const RefCell<State>;
    match msg {
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
        WM_MOUSEACTIVATE if !state.is_null() => {
            // The note does not take focus when clicked.
            if let Ok(state) = unsafe { &*state }.try_borrow()
                && state.is_note()
            {
                return LRESULT(MA_NOACTIVATE as isize);
            }
        }
        WM_TIMER | WM_LBUTTONUP | WM_KEYDOWN | WM_SETCURSOR | WM_CLOSE | WM_APP_LAYOUT
            if !state.is_null() =>
        {
            // Busy when the message comes from inside one of our own calls; then Windows
            // handles it.
            if let Ok(mut state) = unsafe { &*state }.try_borrow_mut()
                && state.on_message(msg, wparam, lparam)
            {
                return LRESULT(if msg == WM_SETCURSOR { 1 } else { 0 });
            }
            // The window is never destroyed, only hidden.
            if msg == WM_CLOSE {
                return LRESULT(0);
            }
        }
        _ => {}
    }
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn warn() -> Content {
        Content::Warn {
            name: Some("Jānis".into()),
            minutes_left: 10,
            reason: Reason::Budget,
        }
    }

    fn time_up() -> Content {
        Content::TimeUp {
            name: None,
            grace_s: 10,
            reason: Reason::Budget,
            can_ask: true,
        }
    }

    fn extra() -> Content {
        Content::ExtraTime {
            minutes: 30,
            by: "Mom".into(),
        }
    }

    #[test]
    fn warnings_and_notes_close_by_themselves() {
        assert_eq!(warn().closes_after(), Some(Duration::from_secs(15)));
        assert_eq!(extra().closes_after(), Some(Duration::from_secs(10)));
        assert_eq!(time_up().closes_after(), None);
    }

    #[test]
    fn time_up_cannot_be_closed() {
        assert!(warn().can_close());
        assert!(extra().can_close());
        assert!(!time_up().can_close());
    }

    #[test]
    fn only_the_note_stays_out_of_the_way() {
        assert!(extra().is_note());
        assert!(!warn().is_note());
        assert!(!time_up().is_note());
    }

    #[test]
    fn time_up_ends_when_time_is_given() {
        assert!(!time_up().ends_with_status(0));
        assert!(!time_up().ends_with_status(-20));
        assert!(time_up().ends_with_status(1800));
        // Other popups close by themselves.
        assert!(!warn().ends_with_status(1800));
        assert!(!extra().ends_with_status(1800));
    }

    #[test]
    fn input_waits_a_moment() {
        assert!(!takes_input(Duration::ZERO));
        assert!(!takes_input(Duration::from_millis(799)));
        assert!(takes_input(Duration::from_millis(800)));
    }

    #[test]
    fn texts_use_the_name_when_known() {
        assert_eq!(
            warn_text(Some("Jānis"), Reason::Budget),
            "Jānis, your screen time for today ends soon. Save what you are doing."
        );
        assert_eq!(
            warn_text(None, Reason::Hours),
            "The allowed hours end soon. Save what you are doing."
        );
        assert_eq!(
            warn_text(Some(" "), Reason::Budget),
            "Your screen time for today ends soon. Save what you are doing."
        );
        assert_eq!(address(None, "ēd"), "Ēd");
        assert_eq!(address(None, ""), "");
    }

    #[test]
    fn time_up_texts() {
        assert_eq!(
            time_up_text(Some("Jānis"), Reason::Budget, true),
            "To ask for more time, send /more 30 to the bot in Telegram."
        );
        assert_eq!(
            time_up_text(Some("Jānis"), Reason::Budget, false),
            "Jānis, your screen time for today is used up."
        );
        assert_eq!(
            time_up_text(None, Reason::Hours, false),
            "Screen time is not allowed at this hour."
        );
    }

    #[test]
    fn warning_numbers() {
        assert_eq!(minutes_left_text(10), "minutes left");
        assert_eq!(minutes_left_text(1), "minute left");
        // The same colours as the timer at that time.
        assert_eq!(warn_level(10), Level::Yellow);
        assert_eq!(warn_level(5), Level::Red);
        assert_eq!(warn_level(1), Level::Red);
    }

    #[test]
    fn countdown_to_sign_out() {
        let s = Duration::from_secs;
        assert_eq!(seconds_left(10, s(0)), 10);
        assert_eq!(seconds_left(10, Duration::from_millis(1500)), 9);
        assert_eq!(seconds_left(10, s(10)), 0);
        assert_eq!(seconds_left(10, s(30)), 0);
        assert_eq!(countdown_text(8), "0:08");
        assert_eq!(countdown_text(75), "1:15");
        assert_eq!(bar_part(10, s(0)), 1.0);
        assert_eq!(bar_part(10, s(2)), 0.8);
        assert_eq!(bar_part(10, s(12)), 0.0);
        assert_eq!(bar_part(0, s(0)), 0.0);
    }

    #[test]
    fn extra_time_texts() {
        assert_eq!(
            extra_texts(30, "Mom"),
            ("+30 minutes".into(), "Mom gave you more time".into())
        );
        assert_eq!(
            extra_texts(1, "Dad"),
            ("+1 minute".into(), "Dad gave you more time".into())
        );
        assert_eq!(
            extra_texts(-15, "Dad"),
            (
                "\u{2212}15 minutes".into(),
                "Dad took some of your time".into()
            )
        );
    }

    #[test]
    fn positions_on_the_screen() {
        let area = RECT {
            left: 1920,
            top: 0,
            right: 3840,
            bottom: 1040,
        };
        let size = SIZE { cx: 400, cy: 300 };
        assert_eq!(centre_pos(area, size), POINT { x: 2680, y: 370 });
        assert_eq!(top_centre_pos(area, size, 24), POINT { x: 2680, y: 24 });
    }

    #[test]
    fn blocks_stack_with_gaps() {
        let (tops, total) = stack(&[16.0, 72.0, 46.0, 44.0], 16.0);
        assert_eq!(tops, [0.0, 32.0, 120.0, 182.0]);
        assert_eq!(total, 226.0);
        assert_eq!(stack(&[], 16.0), (vec![], 0.0));
    }

    #[test]
    fn button_hit_test() {
        let b = rect(272.0, 200.0, 96.0, 44.0);
        assert!(inside(&b, 272.0, 200.0));
        assert!(inside(&b, 367.9, 243.9));
        assert!(!inside(&b, 368.0, 220.0));
        assert!(!inside(&b, 300.0, 199.0));
    }
}
