//! Cursor monitoring and injected mouse actions (via enigo).

use crate::broker::Status;
use crate::state::Shared;
use enigo::{Axis, Button, Coordinate, Direction, Enigo, Key, Keyboard, Mouse, Settings};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

pub type Point = (i32, i32);

const POLL_INTERVAL: Duration = Duration::from_millis(40);

/// `prompt`: allow enigo to open the macOS accessibility-permission prompt.
/// The monitor only reads the cursor, so it passes `false`.
fn enigo(prompt: bool) -> Result<Enigo, String> {
    let settings = Settings {
        open_prompt_to_get_permissions: prompt,
        ..Settings::default()
    };
    Enigo::new(&settings).map_err(|e| format!("cannot access the input system: {e}"))
}

/// Background thread: samples the global cursor position and drives the broker's timers.
pub fn spawn_monitor(shared: Arc<Shared>) {
    thread::Builder::new()
        .name("cursor-monitor".into())
        .spawn(move || {
            let hooked = crate::hook::start();
            shared
                .hook_active
                .store(hooked, std::sync::atomic::Ordering::Relaxed);
            let mut last_pos: Option<(i32, i32)> = None;
            let mut was_near = false;
            let mut e = None;
            loop {
                if e.is_none() {
                    match enigo(false) {
                        Ok(x) => e = Some(x),
                        Err(err) => shared.set_cursor_error(Some(err)),
                    }
                }
                let pos = match e.as_ref().map(|e| e.location()) {
                    Some(Ok(p)) => {
                        shared.set_cursor_error(None);
                        Some(p)
                    }
                    Some(Err(err)) => {
                        shared
                            .set_cursor_error(Some(format!("cannot read cursor position: {err}")));
                        None
                    }
                    None => None,
                };
                // Always drain the hook so stale input doesn't pile up.
                let activity = crate::hook::take();
                if let Some(p) = pos {
                    // When idle the bell doesn't redraw by itself, so it relies on
                    // this to notice the cursor reaching it (to stop being
                    // click-through) and leaving it again.
                    let near = shared.cursor_near_window(p);
                    if near != was_near || (near && last_pos != Some(p)) {
                        shared.repaint();
                    }
                    was_near = near;
                    if shared.hardware_detection() {
                        let mut delta = (activity.hw_dx, activity.hw_dy);
                        // The cursor moved but no input event explains it
                        // (SetCursorPos, or input hidden from the hook).
                        let silent = activity.hw_events == 0 && activity.injected_events == 0;
                        if silent && last_pos.is_some_and(|lp| lp != p) {
                            crate::hook::resync(p);
                            if shared.config.lock().unwrap().unexplained_moves_are_user {
                                let lp = last_pos.unwrap();
                                delta = (p.0 - lp.0, p.1 - lp.1);
                            }
                        }
                        let typed = activity.hw_keys > 0
                            && shared.config.lock().unwrap().keyboard_takes_back;
                        shared.with(|b, now| {
                            b.observe_cursor(p);
                            b.on_user_input(delta, activity.hw_buttons > 0 || typed, now);
                        });
                    } else {
                        shared.with(|b, now| b.on_cursor(p, now));
                    }
                    last_pos = Some(p);
                } else {
                    shared.with(|_, _| {});
                }
                thread::sleep(POLL_INTERVAL);
            }
        })
        .expect("spawn monitor thread");
}

#[derive(Debug, Clone, Copy)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
}

impl MouseButton {
    pub fn parse(s: Option<&str>) -> Result<Self, String> {
        match s.unwrap_or("left") {
            "left" => Ok(Self::Left),
            "right" => Ok(Self::Right),
            "middle" => Ok(Self::Middle),
            other => Err(format!(
                "unknown button '{other}' (use left, right or middle)"
            )),
        }
    }
    fn enigo(self) -> Button {
        match self {
            Self::Left => Button::Left,
            Self::Right => Button::Right,
            Self::Middle => Button::Middle,
        }
    }
}

#[derive(Debug, Clone)]
pub enum Action {
    Move {
        x: i32,
        y: i32,
    },
    Click {
        at: Option<(i32, i32)>,
        button: MouseButton,
        count: u32,
    },
    Drag {
        from: (i32, i32),
        to: (i32, i32),
        button: MouseButton,
    },
    Scroll {
        at: Option<(i32, i32)>,
        amount: i32,
        horizontal: bool,
    },
    Type {
        text: String,
    },
    /// A key combination: all but the last key are held while the last is pressed.
    Keys {
        keys: Vec<Key>,
        repeat: u32,
    },
}

#[derive(Debug, PartialEq)]
enum Segment<'a> {
    Text(&'a str),
    Key(Key),
}

/// Split text into plain runs and Enter/Tab key presses. enigo's own `text()`
/// sends '\n' and '\t' both as a key press *and* as a character on Windows,
/// which doubles them, so we never pass those characters to it.
fn segments(text: &str) -> Vec<Segment<'_>> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut prev_cr = false;
    for (i, c) in text.char_indices() {
        let key = match c {
            '\r' => Some(Key::Return),
            '\n' if prev_cr => None, // "\r\n" is one line break
            '\n' => Some(Key::Return),
            '\t' => Some(Key::Tab),
            _ => {
                prev_cr = false;
                continue;
            }
        };
        if start < i {
            out.push(Segment::Text(&text[start..i]));
        }
        if let Some(k) = key {
            out.push(Segment::Key(k));
        }
        prev_cr = c == '\r';
        start = i + c.len_utf8();
    }
    if start < text.len() {
        out.push(Segment::Text(&text[start..]));
    }
    out
}

/// Parse a combination such as `ctrl+shift+t`, `enter`, `alt+f4` or `a`.
pub fn parse_keys(combo: &str) -> Result<Vec<Key>, String> {
    let mut keys = Vec::new();
    // "ctrl++" means ctrl and the plus key
    let combo = combo.trim();
    let mut parts: Vec<&str> = combo.split('+').map(str::trim).collect();
    if combo.ends_with("++") {
        parts.truncate(parts.len() - 2);
        parts.push("+");
    }
    for part in parts {
        if part.is_empty() {
            return Err(format!("invalid key combination '{combo}'"));
        }
        keys.push(parse_key(part)?);
    }
    if keys.is_empty() {
        return Err("no keys given".into());
    }
    Ok(keys)
}

fn parse_key(name: &str) -> Result<Key, String> {
    let lower = name.to_ascii_lowercase();
    let key = match lower.as_str() {
        "ctrl" | "control" => Key::Control,
        "shift" => Key::Shift,
        "alt" | "option" | "opt" => Key::Alt,
        "meta" | "win" | "windows" | "super" | "cmd" | "command" => Key::Meta,
        "enter" | "return" => Key::Return,
        "tab" => Key::Tab,
        "esc" | "escape" => Key::Escape,
        "backspace" => Key::Backspace,
        "delete" | "del" => Key::Delete,
        "space" => Key::Space,
        "up" | "arrowup" => Key::UpArrow,
        "down" | "arrowdown" => Key::DownArrow,
        "left" | "arrowleft" => Key::LeftArrow,
        "right" | "arrowright" => Key::RightArrow,
        "home" => Key::Home,
        "end" => Key::End,
        "pageup" | "pgup" => Key::PageUp,
        "pagedown" | "pgdn" => Key::PageDown,
        "capslock" => Key::CapsLock,
        #[cfg(not(target_os = "macos"))]
        "insert" | "ins" => Key::Insert,
        #[cfg(not(target_os = "macos"))]
        "printscreen" | "prtsc" => Key::PrintScr,
        "f1" => Key::F1,
        "f2" => Key::F2,
        "f3" => Key::F3,
        "f4" => Key::F4,
        "f5" => Key::F5,
        "f6" => Key::F6,
        "f7" => Key::F7,
        "f8" => Key::F8,
        "f9" => Key::F9,
        "f10" => Key::F10,
        "f11" => Key::F11,
        "f12" => Key::F12,
        _ => {
            let mut chars = name.chars();
            match (chars.next(), chars.next()) {
                // Shortcuts use the unshifted key: ctrl+A means ctrl+a.
                (Some(c), None) => Key::Unicode(c.to_ascii_lowercase()),
                _ => return Err(format!("unknown key '{name}'")),
            }
        }
    };
    Ok(key)
}

fn sleep_ms(ms: u64) {
    thread::sleep(Duration::from_millis(ms));
}

fn run(e: &mut Enigo, action: &Action) -> Result<(), enigo::InputError> {
    match *action {
        Action::Type { ref text } => {
            for seg in segments(text) {
                match seg {
                    Segment::Text(t) => e.text(t)?,
                    Segment::Key(k) => e.key(k, Direction::Click)?,
                }
                sleep_ms(5);
            }
        }
        Action::Keys { ref keys, repeat } => {
            let (last, held) = keys
                .split_last()
                .expect("parse_keys returns at least one key");
            for i in 0..repeat.clamp(1, 50) {
                if i > 0 {
                    sleep_ms(30);
                }
                for k in held {
                    e.key(*k, Direction::Press)?;
                }
                let r = e.key(*last, Direction::Click);
                // Always release the modifiers, even if the main key failed.
                for k in held.iter().rev() {
                    e.key(*k, Direction::Release)?;
                }
                r?;
            }
        }
        Action::Move { x, y } => e.move_mouse(x, y, Coordinate::Abs)?,
        Action::Click { at, button, count } => {
            if let Some((x, y)) = at {
                e.move_mouse(x, y, Coordinate::Abs)?;
                sleep_ms(30);
            }
            for i in 0..count.clamp(1, 3) {
                if i > 0 {
                    sleep_ms(40);
                }
                e.button(button.enigo(), Direction::Click)?;
            }
        }
        Action::Drag { from, to, button } => {
            e.move_mouse(from.0, from.1, Coordinate::Abs)?;
            sleep_ms(50);
            e.button(button.enigo(), Direction::Press)?;
            sleep_ms(50);
            const STEPS: i32 = 12;
            for i in 1..=STEPS {
                let x = from.0 + (to.0 - from.0) * i / STEPS;
                let y = from.1 + (to.1 - from.1) * i / STEPS;
                e.move_mouse(x, y, Coordinate::Abs)?;
                sleep_ms(16);
            }
            sleep_ms(50);
            e.button(button.enigo(), Direction::Release)?;
        }
        Action::Scroll {
            at,
            amount,
            horizontal,
        } => {
            if let Some((x, y)) = at {
                e.move_mouse(x, y, Coordinate::Abs)?;
                sleep_ms(30);
            }
            let axis = if horizontal {
                Axis::Horizontal
            } else {
                Axis::Vertical
            };
            e.scroll(amount, axis)?;
        }
    }
    Ok(())
}

/// Perform an action on behalf of `session`, which must hold the lease.
/// Returns the cursor position afterwards.
pub fn perform(
    shared: &Shared,
    session: &str,
    action: &Action,
) -> Result<(i32, i32), PerformError> {
    shared
        .with(|b, now| b.begin_action(session, now))
        .map_err(PerformError::NoLease)?;
    let mut acting = Acting { shared, pos: None };
    let result = enigo(true).and_then(|mut e| {
        run(&mut e, action).map_err(|err| format!("input simulation failed: {err}"))?;
        e.location()
            .map_err(|err| format!("cannot read cursor position: {err}"))
    });
    acting.pos = result.as_ref().ok().copied();
    drop(acting);
    result.map_err(PerformError::Input)
}

/// Ends an action when dropped, even if input simulation panics: otherwise
/// the broker would think an action is still running and, in movement mode,
/// ignore the user's mouse for good.
struct Acting<'a> {
    shared: &'a Shared,
    pos: Option<Point>,
}

impl Drop for Acting<'_> {
    fn drop(&mut self) {
        let pos = self.pos;
        self.shared.with(|b, now| b.end_action(pos, now));
    }
}

pub enum PerformError {
    NoLease(Status),
    Input(String),
}

/// Returns (main display size, cursor position).
pub fn screen_info() -> Result<(Point, Point), String> {
    let e = enigo(false)?;
    let size = e.main_display().map_err(|err| err.to_string())?;
    let pos = e.location().map_err(|err| err.to_string())?;
    Ok((size, pos))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_key_combinations() {
        assert_eq!(
            parse_keys("ctrl+s").unwrap(),
            vec![Key::Control, Key::Unicode('s')]
        );
        assert_eq!(
            parse_keys(" Ctrl + Shift + T ").unwrap(),
            vec![Key::Control, Key::Shift, Key::Unicode('t')]
        );
        assert_eq!(parse_keys("enter").unwrap(), vec![Key::Return]);
        assert_eq!(parse_keys("alt+F4").unwrap(), vec![Key::Alt, Key::F4]);
        assert_eq!(
            parse_keys("ctrl++").unwrap(),
            vec![Key::Control, Key::Unicode('+')]
        );
        assert!(parse_keys("ctrl+").is_err());
        assert!(parse_keys("hyper+x").is_err());
        assert!(parse_keys("").is_err());
    }

    #[test]
    fn splits_text_into_runs_and_keys() {
        use Segment::{Key as K, Text};
        assert_eq!(
            segments("Héllo ✓\nline2\tx"),
            vec![
                Text("Héllo ✓"),
                K(Key::Return),
                Text("line2"),
                K(Key::Tab),
                Text("x")
            ]
        );
        assert_eq!(
            segments("a\r\nb"),
            vec![Text("a"), K(Key::Return), Text("b")]
        );
        assert_eq!(segments("\n\n"), vec![K(Key::Return), K(Key::Return)]);
        assert_eq!(segments(""), vec![]);
    }
}
