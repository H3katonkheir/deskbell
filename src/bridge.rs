//! `deskbell mcp`: a stdio MCP server that forwards every message to the
//! running bell widget over HTTP, starting the widget if needed.
//! This is what you register in MCP clients that only speak stdio.

use crate::http::SESSION_HEADER;
use crate::mcp;
use serde_json::Value;
use std::io::{BufRead, Write};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

struct Bridge {
    agent: ureq::Agent,
    url: String,
    session: Mutex<Option<String>>,
    /// The original initialize message, replayed if the widget restarts.
    init_msg: Mutex<Option<Value>>,
    stdout: Mutex<std::io::Stdout>,
}

enum PostError {
    Unreachable(String),
    SessionLost,
}

impl Bridge {
    fn post(&self, msg: &Value) -> Result<Option<String>, PostError> {
        let mut req = self
            .agent
            .post(&self.url)
            .header("Content-Type", "application/json")
            .header("Accept", "application/json, text/event-stream");
        if let Some(s) = self.session.lock().unwrap().clone() {
            req = req.header(SESSION_HEADER, s);
        }
        let mut resp = req
            .send(msg.to_string())
            .map_err(|e| PostError::Unreachable(e.to_string()))?;
        if resp.status().as_u16() == 404 {
            return Err(PostError::SessionLost);
        }
        if let Some(s) = resp
            .headers()
            .get(SESSION_HEADER)
            .and_then(|v| v.to_str().ok())
        {
            *self.session.lock().unwrap() = Some(s.to_string());
        }
        let body = resp
            .body_mut()
            .read_to_string()
            .map_err(|e| PostError::Unreachable(e.to_string()))?;
        Ok((!body.trim().is_empty()).then_some(body))
    }

    fn reinitialize(&self) -> bool {
        *self.session.lock().unwrap() = None;
        let Some(init) = self.init_msg.lock().unwrap().clone() else {
            return false;
        };
        if self.post(&init).is_err() {
            return false;
        }
        let note = serde_json::json!({ "jsonrpc": "2.0", "method": "notifications/initialized" });
        let _ = self.post(&note);
        true
    }

    /// Tell the widget this session is over, so a queued request or a lease
    /// doesn't outlive the agent (the bell would keep ringing for it).
    fn end_session(&self) {
        let Some(session) = self.session.lock().unwrap().take() else {
            return;
        };
        let _ = self
            .agent
            .delete(&self.url)
            .config()
            .timeout_global(Some(Duration::from_secs(2)))
            .build()
            .header(SESSION_HEADER, session)
            .call();
    }

    fn write(&self, line: &str) {
        let mut out = self.stdout.lock().unwrap();
        let _ = writeln!(out, "{}", line.trim());
        let _ = out.flush();
    }

    fn forward(&self, msg: Value) {
        let mut result = self.post(&msg);
        if matches!(result, Err(PostError::SessionLost)) && self.reinitialize() {
            result = self.post(&msg);
        }
        match result {
            Ok(Some(body)) => self.write(&body),
            Ok(None) => {}
            Err(e) => {
                if let Some(id) = msg.get("id").filter(|_| msg.get("method").is_some()) {
                    let why = match e {
                        PostError::Unreachable(e) => format!(
                            "The deskbell widget is not running or unreachable ({e}). \
                             Ask the user to start it."
                        ),
                        PostError::SessionLost => "Session lost".to_string(),
                    };
                    self.write(&mcp::rpc_error(id, -32000, &why).to_string());
                }
            }
        }
    }
}

fn widget_alive(agent: &ureq::Agent, port: u16) -> bool {
    agent
        .get(format!("http://127.0.0.1:{port}/status"))
        .call()
        .is_ok_and(|r| r.status().is_success())
}

/// Windows' CreateProcess inherits every inheritable handle, including the
/// pipes our MCP client gave us as stdio. If the long-lived widget inherited
/// them, the client would never see EOF on our stdout after we exit.
#[cfg(windows)]
fn make_stdio_uninheritable() {
    use std::ffi::c_void;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetStdHandle(n: u32) -> *mut c_void;
        fn SetHandleInformation(h: *mut c_void, mask: u32, flags: u32) -> i32;
    }
    const HANDLE_FLAG_INHERIT: u32 = 1;
    for n in [-10i32, -11, -12] {
        // SAFETY: plain Win32 calls on our own std handles.
        unsafe {
            let h = GetStdHandle(n as u32);
            if !h.is_null() && h as isize != -1 {
                SetHandleInformation(h, HANDLE_FLAG_INHERIT, 0);
            }
        }
    }
}

fn launch_widget() {
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    #[cfg(windows)]
    make_stdio_uninheritable();
    let mut cmd = Command::new(exe);
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
        cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_BREAKAWAY_FROM_JOB);
        if cmd.spawn().is_ok() {
            return;
        }
        cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
    }
    let _ = cmd.spawn();
}

pub fn run(port: u16, agent_name: Option<String>, autostart: bool) {
    let config = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_global(None)
        .timeout_connect(Some(Duration::from_secs(2)))
        .build();
    let agent: ureq::Agent = config.into();

    if autostart && !widget_alive(&agent, port) {
        launch_widget();
        let deadline = Instant::now() + Duration::from_secs(15);
        while Instant::now() < deadline && !widget_alive(&agent, port) {
            std::thread::sleep(Duration::from_millis(250));
        }
    }

    let bridge = Arc::new(Bridge {
        agent,
        url: format!("http://127.0.0.1:{port}/mcp"),
        session: Mutex::new(None),
        init_msg: Mutex::new(None),
        stdout: Mutex::new(std::io::stdout()),
    });

    let stdin = std::io::stdin();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let mut msg: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                bridge.write(
                    &mcp::rpc_error(&Value::Null, -32700, &format!("Parse error: {e}")).to_string(),
                );
                continue;
            }
        };
        if msg.get("method").and_then(Value::as_str) == Some("initialize") {
            if let Some(name) = &agent_name
                && let Some(params) = msg.get_mut("params").and_then(Value::as_object_mut)
            {
                params.insert(
                    "clientInfo".into(),
                    serde_json::json!({ "name": name, "version": "0" }),
                );
            }
            *bridge.init_msg.lock().unwrap() = Some(msg.clone());
            // Must complete before anything else so the session id is known.
            bridge.forward(msg);
        } else {
            // Tool calls can block for a long time; keep reading stdin meanwhile.
            let b = bridge.clone();
            std::thread::spawn(move || b.forward(msg));
        }
    }
    // stdin closed: the MCP client is gone. Calls still in flight die with
    // this process; deleting the session also ends any request_control
    // that is blocked waiting in the widget.
    bridge.end_session();
}
