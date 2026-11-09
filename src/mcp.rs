//! Minimal MCP (Model Context Protocol) server: JSON-RPC 2.0 messages,
//! `initialize`, `tools/list`, `tools/call`, `ping`.

use crate::broker::{Ended, GrantReason, Status};
use crate::input::{self, Action, MouseButton, PerformError};
use crate::state::Shared;
use serde_json::{Value, json};
use std::time::Duration;

const SUPPORTED_VERSIONS: &[&str] = &["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];
const DEFAULT_WAIT_SECS: u64 = 45;
const MAX_WAIT_SECS: u64 = 600;
const MAX_TEXT_CHARS: usize = 10_000;
/// Far beyond any desktop, but small enough that coordinate maths can't overflow.
const MAX_COORD: i32 = 100_000;
const MAX_SCROLL: i32 = 100;
pub const MAX_AGENT_CHARS: usize = 60;
const MAX_TASK_CHARS: usize = 300;

/// Text an agent chose that the bell shows the user (its name, its task), as
/// a single line: control characters (newlines included) and bidi overrides
/// could otherwise fake extra lines or reorder what the bubble says.
pub fn clean_label(s: &str, max_chars: usize) -> String {
    let spaced: String = s
        .chars()
        .map(|c| {
            let bidi = matches!(c, '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}');
            if c.is_control() || bidi { ' ' } else { c }
        })
        .collect();
    let one_line = spaced.split_whitespace().collect::<Vec<_>>().join(" ");
    one_line.chars().take(max_chars).collect()
}

const INSTRUCTIONS: &str = "\
This server (deskbell) arbitrates access to the user's PHYSICAL mouse and keyboard, which the user may be using at \
the same time. Rules: (1) Before ANY mouse or keyboard automation call `request_control` with a short description of \
your task; the user sees a ringing bell with your name and task and must approve (or stay idle for a while, which \
auto-approves). (2) Only move, click or type while you hold the lease. (3) A lease lasts about a minute: call \
`renew_control` before it runs out if you need longer, and `release_control` as soon as you're done. (4) If the user \
takes the mouse back you lose access immediately; stop and call `request_control` again (you'll be first in line). \
Never fight the user for control. Never click the bell widget yourself.";

const HARDWARE: &str = " (5) This machine distinguishes the user's physical mouse and keyboard from software input, \
so while you hold the lease you MAY use your own mouse/keyboard/computer-use tools as well as this server's tools. \
Exception: tools that teleport the cursor with SetCursorPos (e.g. pyautogui.moveTo) may be mistaken for the user.";

const MOVEMENT_ONLY: &str = " (5) Use this server's mouse_*, type_text and press_keys tools for all input: on this \
machine, cursor movement by any other tool cannot be told apart from the user's and will immediately revoke your \
access. The user's typing can't be detected here, so only type right after clicking into the target field.";

/// Agent-facing usage rules, depending on whether hardware detection is active.
pub fn instructions(hardware_detection: bool) -> String {
    let extra = if hardware_detection {
        HARDWARE
    } else {
        MOVEMENT_ONLY
    };
    format!("{INSTRUCTIONS}{extra}")
}

pub fn negotiate_version(params: &Value) -> &'static str {
    let requested = params.get("protocolVersion").and_then(Value::as_str);
    SUPPORTED_VERSIONS
        .iter()
        .find(|v| Some(**v) == requested)
        .copied()
        .unwrap_or(SUPPORTED_VERSIONS[0])
}

pub fn initialize_result(params: &Value, hardware_detection: bool) -> Value {
    json!({
        "protocolVersion": negotiate_version(params),
        "capabilities": { "tools": { "listChanged": false } },
        "serverInfo": {
            "name": "deskbell",
            "title": "Deskbell",
            "version": env!("CARGO_PKG_VERSION"),
        },
        "instructions": instructions(hardware_detection),
    })
}

fn rpc_result(id: &Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

pub fn rpc_error(id: &Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

/// Handle one JSON-RPC message for an established session.
/// Returns `None` for notifications / responses (nothing to send back).
pub fn handle(shared: &Shared, session: &str, msg: &Value) -> Option<Value> {
    let id = msg.get("id")?.clone();
    let Some(method) = msg.get("method").and_then(Value::as_str) else {
        return None; // a response from the client; we never send requests
    };
    let params = msg.get("params").cloned().unwrap_or(Value::Null);
    Some(match method {
        "initialize" => rpc_result(&id, initialize_result(&params, shared.hardware_detection())),
        "ping" => rpc_result(&id, json!({})),
        "tools/list" => rpc_result(&id, json!({ "tools": tool_list() })),
        "tools/call" => {
            let name = params.get("name").and_then(Value::as_str).unwrap_or("");
            let args = params.get("arguments").cloned().unwrap_or(json!({}));
            match call_tool(shared, session, name, &args) {
                Some(r) => rpc_result(&id, r),
                None => rpc_error(&id, -32602, &format!("Unknown tool: {name}")),
            }
        }
        "resources/list" => rpc_result(&id, json!({ "resources": [] })),
        "prompts/list" => rpc_result(&id, json!({ "prompts": [] })),
        _ => rpc_error(&id, -32601, &format!("Method not found: {method}")),
    })
}

fn text(s: impl Into<String>) -> Value {
    json!({ "content": [{ "type": "text", "text": s.into() }], "isError": false })
}

fn error(s: impl Into<String>) -> Value {
    json!({ "content": [{ "type": "text", "text": s.into() }], "isError": true })
}

fn secs(d: Duration) -> u64 {
    d.as_secs_f32().round() as u64
}

fn describe(status: &Status) -> String {
    match status {
        Status::Holding { remaining, reason } => format!(
            "GRANTED{}: you control the mouse and keyboard for the next {}s. Call renew_control before it expires \
             if you need more time and release_control when done. If the user takes the mouse back you lose \
             access immediately.",
            match reason {
                GrantReason::UserClick => " (approved by the user)",
                GrantReason::Idle => " (auto-approved: the user was idle)",
                GrantReason::AutoFire => " (auto-approved: the user enabled auto-fire)",
            },
            secs(*remaining)
        ),
        Status::Queued {
            position,
            auto_grant_in,
        } => {
            let mut s = if *position == 1 {
                "WAITING: the bell is ringing for you; waiting for the user to approve".to_string()
            } else {
                format!("WAITING: you are #{position} in the queue")
            };
            if let Some(d) = auto_grant_in {
                s += &format!(
                    " (auto-approval in ~{}s if the user stays idle)",
                    secs(*d)
                );
            }
            s + ". Call request_control again to keep waiting, or release_control to give up. \
                 Do NOT touch the mouse or keyboard yet."
        }
        Status::Idle { last } => match last {
            None => "NOT HOLDING: you have not requested control. Call request_control first.".into(),
            Some(Ended::Released) => "NOT HOLDING: you released control.".into(),
            Some(Ended::Expired) => {
                "NOT HOLDING: your lease expired. Call request_control to ask again.".into()
            }
            Some(Ended::RevokedByMovement) => "NOT HOLDING: the user took the mouse/keyboard back, so your \
                access was revoked. Stop immediately. Call request_control to ask again (you will be first in \
                line)."
                .into(),
            Some(Ended::RevokedByUser) => {
                "NOT HOLDING: the user took control back. Stop using the mouse and keyboard.".into()
            }
            Some(Ended::Denied) => "DENIED: the user declined your request. Do not use the mouse or keyboard; \
                continue without them or ask the user what they prefer."
                .into(),
            Some(Ended::Stale) => "NOT HOLDING: your queued request timed out because you stopped polling. \
                Call request_control again."
                .into(),
        },
    }
}

fn arg_i32(args: &Value, key: &str) -> Result<Option<i32>, String> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v
            .as_f64()
            .filter(|f| f.abs() <= MAX_COORD as f64)
            .map(|f| Some(f.round() as i32))
            .ok_or_else(|| {
                format!("'{key}' must be a number between -{MAX_COORD} and {MAX_COORD}")
            }),
    }
}

fn req_i32(args: &Value, key: &str) -> Result<i32, String> {
    arg_i32(args, key)?.ok_or_else(|| format!("missing required argument '{key}'"))
}

fn req_str<'a>(args: &'a Value, key: &str) -> Result<&'a str, String> {
    args.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("missing required argument '{key}'"))
}

fn opt_point(args: &Value) -> Result<Option<(i32, i32)>, String> {
    match (arg_i32(args, "x")?, arg_i32(args, "y")?) {
        (Some(x), Some(y)) => Ok(Some((x, y))),
        (None, None) => Ok(None),
        _ => Err("give both 'x' and 'y' or neither".into()),
    }
}

fn parse_action(name: &str, args: &Value) -> Result<Option<Action>, String> {
    let button = || MouseButton::parse(args.get("button").and_then(Value::as_str));
    Ok(Some(match name {
        "mouse_move" => Action::Move {
            x: req_i32(args, "x")?,
            y: req_i32(args, "y")?,
        },
        "mouse_click" => Action::Click {
            at: opt_point(args)?,
            button: button()?,
            count: arg_i32(args, "count")?.unwrap_or(1).clamp(1, 3) as u32,
        },
        "mouse_drag" => Action::Drag {
            from: (req_i32(args, "from_x")?, req_i32(args, "from_y")?),
            to: (req_i32(args, "to_x")?, req_i32(args, "to_y")?),
            button: button()?,
        },
        "mouse_scroll" => Action::Scroll {
            at: opt_point(args)?,
            amount: req_i32(args, "amount")?.clamp(-MAX_SCROLL, MAX_SCROLL),
            horizontal: args
                .get("horizontal")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        },
        "type_text" => {
            let t = req_str(args, "text")?;
            if t.chars().count() > MAX_TEXT_CHARS {
                return Err(format!(
                    "text is longer than {MAX_TEXT_CHARS} characters; split it up"
                ));
            }
            Action::Type {
                text: t.to_string(),
            }
        }
        "press_keys" => Action::Keys {
            keys: input::parse_keys(req_str(args, "keys")?)?,
            repeat: arg_i32(args, "repeat")?.unwrap_or(1).clamp(1, 50) as u32,
        },
        _ => return Ok(None),
    }))
}

fn call_tool(shared: &Shared, session: &str, name: &str, args: &Value) -> Option<Value> {
    Some(match name {
        "request_control" => {
            let task = args
                .get("task")
                .and_then(Value::as_str)
                .map(|t| clean_label(t, MAX_TASK_CHARS))
                .unwrap_or_default();
            if task.is_empty() {
                return Some(error("missing required argument 'task'"));
            }
            let wait = args
                .get("wait_seconds")
                .and_then(Value::as_u64)
                .unwrap_or(DEFAULT_WAIT_SECS)
                .min(MAX_WAIT_SECS);
            let agent = {
                let mut sessions = shared.sessions.lock().unwrap();
                let s = sessions.get_mut(session)?;
                if let Some(n) = args
                    .get("agent_name")
                    .and_then(Value::as_str)
                    .map(|n| clean_label(n, MAX_AGENT_CHARS))
                    .filter(|n| !n.is_empty())
                {
                    s.agent_name = Some(n);
                }
                s.display_name()
            };
            let st = shared.request_and_wait(session, &agent, &task, Duration::from_secs(wait));
            text(describe(&st))
        }
        "renew_control" => match shared.with(|b, now| b.renew(session, now)) {
            Ok(d) => text(format!(
                "RENEWED: you control the mouse and keyboard for the next {}s.",
                secs(d)
            )),
            Err(st) => error(describe(&st)),
        },
        "release_control" => {
            if shared.with(|b, now| b.release(session, now)) {
                text("RELEASED: thanks! The user is back in control.")
            } else {
                text("You were neither holding nor waiting for control.")
            }
        }
        "control_status" => {
            let st = shared.with(|b, now| b.poll(session, now));
            let (queue_len, holder) =
                shared.with(|b, _| (b.queue().len(), b.holder().map(|l| l.agent.clone())));
            let mut s = describe(&st);
            s += &format!("\nQueue length: {queue_len}.");
            if let (Some(h), false) = (holder, matches!(st, Status::Holding { .. })) {
                s += &format!(" Currently held by: {h}.");
            }
            s += if shared.hardware_detection() {
                " Your own mouse/keyboard tools are allowed while you hold the lease (except SetCursorPos-style \
                 teleports)."
            } else {
                " Use only this server's mouse_*, type_text and press_keys tools: other cursor movement counts as \
                 the user's."
            };
            text(s)
        }
        "get_screen_info" => match input::screen_info() {
            Ok(((w, h), (x, y))) => text(format!(
                "Main display: {w}x{h} pixels. Cursor at ({x}, {y}). Coordinates are absolute, origin at the \
                 top-left of the main display ({}).",
                if cfg!(target_os = "macos") {
                    "logical points"
                } else {
                    "physical pixels"
                }
            )),
            Err(e) => error(e),
        },
        _ => match parse_action(name, args) {
            Err(e) => error(e),
            Ok(None) => return None,
            Ok(Some(action)) => match input::perform(shared, session, &action) {
                Ok((x, y)) => {
                    let left = shared
                        .with(|b, now| b.lease_remaining(now))
                        .map(secs)
                        .unwrap_or(0);
                    text(format!(
                        "OK. Cursor now at ({x}, {y}). Lease: {left}s left."
                    ))
                }
                Err(PerformError::NoLease(st)) => error(describe(&st)),
                Err(PerformError::Input(e)) => error(e),
            },
        },
    })
}

fn tool_list() -> Value {
    let button =
        json!({ "type": "string", "enum": ["left", "right", "middle"], "default": "left" });
    let int = |d: &str| json!({ "type": "integer", "description": d });
    json!([
        {
            "name": "request_control",
            "title": "Request control",
            "description": "Ring the user's desk bell to ask for control of their mouse and keyboard. Shows your \
                name and task in a speech bubble; the user approves by clicking the bell, or approval happens \
                automatically if they leave the computer alone for a while. Blocks up to `wait_seconds` and \
                returns GRANTED, WAITING (call again to keep waiting; poll at least every 2 minutes or you lose \
                your place) or DENIED.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "task": { "type": "string", "description": "Short description of what you need the mouse/keyboard for, shown to the user." },
                    "agent_name": { "type": "string", "description": "Name to show the user (defaults to your MCP client name)." },
                    "wait_seconds": { "type": "integer", "minimum": 0, "maximum": MAX_WAIT_SECS, "default": DEFAULT_WAIT_SECS,
                        "description": "How long to block waiting for a decision (0 = return immediately)." }
                },
                "required": ["task"]
            }
        },
        {
            "name": "renew_control",
            "title": "Renew lease",
            "description": "Extend your lease by another full lease period. Fails if you no longer hold control.",
            "inputSchema": { "type": "object", "properties": {} }
        },
        {
            "name": "release_control",
            "title": "Release control",
            "description": "Give the mouse and keyboard back to the user (or leave the queue). Always call this when done.",
            "inputSchema": { "type": "object", "properties": {} }
        },
        {
            "name": "control_status",
            "title": "Control status",
            "description": "Check whether you hold control, your queue position, remaining lease time, and which \
                input tools you may use on this machine.",
            "inputSchema": { "type": "object", "properties": {} },
            "annotations": { "readOnlyHint": true }
        },
        {
            "name": "get_screen_info",
            "title": "Screen info",
            "description": "Main display size and current cursor position. Does not require a lease.",
            "inputSchema": { "type": "object", "properties": {} },
            "annotations": { "readOnlyHint": true }
        },
        {
            "name": "mouse_move",
            "title": "Move mouse",
            "description": "Move the cursor to absolute screen coordinates. Requires the lease.",
            "inputSchema": { "type": "object", "properties": { "x": int("X coordinate"), "y": int("Y coordinate") }, "required": ["x", "y"] }
        },
        {
            "name": "mouse_click",
            "title": "Click",
            "description": "Click (optionally at x,y first). count=2 for double-click. Requires the lease.",
            "inputSchema": { "type": "object", "properties": {
                "x": int("Optional X coordinate to move to first"), "y": int("Optional Y coordinate to move to first"),
                "button": button, "count": { "type": "integer", "minimum": 1, "maximum": 3, "default": 1 }
            } }
        },
        {
            "name": "mouse_drag",
            "title": "Drag",
            "description": "Press at (from_x, from_y), move to (to_x, to_y) and release. Requires the lease.",
            "inputSchema": { "type": "object", "properties": {
                "from_x": int("Start X"), "from_y": int("Start Y"), "to_x": int("End X"), "to_y": int("End Y"), "button": button
            }, "required": ["from_x", "from_y", "to_x", "to_y"] }
        },
        {
            "name": "mouse_scroll",
            "title": "Scroll",
            "description": "Scroll the wheel by `amount` notches (positive = down/right), optionally at x,y first. Requires the lease.",
            "inputSchema": { "type": "object", "properties": {
                "amount": { "type": "integer", "minimum": -MAX_SCROLL, "maximum": MAX_SCROLL,
                    "description": "Wheel notches; positive scrolls down (or right if horizontal)" },
                "horizontal": { "type": "boolean", "default": false },
                "x": int("Optional X coordinate to move to first"), "y": int("Optional Y coordinate to move to first")
            }, "required": ["amount"] }
        },
        {
            "name": "type_text",
            "title": "Type text",
            "description": "Type text into the focused window (click into the target field first). Unicode is \
                supported; use \\n for Enter. Requires the lease.",
            "inputSchema": { "type": "object", "properties": {
                "text": { "type": "string", "description": "Text to type (max 10000 characters)." }
            }, "required": ["text"] }
        },
        {
            "name": "press_keys",
            "title": "Press keys",
            "description": "Press a key or shortcut, e.g. \"enter\", \"ctrl+s\", \"ctrl+shift+t\", \"alt+tab\", \
                \"pagedown\", \"f5\". Modifiers: ctrl, shift, alt (option), meta (win/cmd). Named keys: enter, \
                tab, esc, backspace, delete, space, up, down, left, right, home, end, pageup, pagedown, f1-f12; \
                otherwise a single character. Requires the lease.",
            "inputSchema": { "type": "object", "properties": {
                "keys": { "type": "string", "description": "Key or '+'-separated combination." },
                "repeat": { "type": "integer", "minimum": 1, "maximum": 50, "default": 1, "description": "Press it this many times." }
            }, "required": ["keys"] }
        }
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_are_one_clean_line() {
        assert_eq!(clean_label("  Claude\n  (repo X) ", 60), "Claude (repo X)");
        assert_eq!(
            clean_label("Fill form\nhas control\r\n\tClick to allow", 300),
            "Fill form has control Click to allow"
        );
        assert_eq!(clean_label("a\u{202E}cba\u{2066}b\u{0007}", 60), "a cba b");
        assert_eq!(clean_label("\n\t ", 60), "");
        assert_eq!(clean_label("héllo wörld", 5), "héllo");
    }

    #[test]
    fn coordinates_and_scrolling_are_bounded() {
        let ok = json!({ "x": -1920, "y": 99999.6 });
        assert_eq!(opt_point(&ok), Ok(Some((-1920, 100_000))));
        for bad in [
            json!({ "x": 1e12, "y": 0 }),
            json!({ "x": 0, "y": -100_001 }),
        ] {
            assert!(opt_point(&bad).is_err(), "{bad}");
        }
        let Ok(Some(Action::Scroll { amount, .. })) =
            parse_action("mouse_scroll", &json!({ "amount": -5000 }))
        else {
            panic!("scroll should parse")
        };
        assert_eq!(amount, -MAX_SCROLL);
    }
}
