//! Local HTTP server exposing the MCP "Streamable HTTP" transport at
//! `http://127.0.0.1:<port>/mcp` (JSON responses only), plus `/status`.

use crate::mcp;
use crate::state::{Session, Shared};
use serde_json::{Value, json};
use std::hash::{BuildHasher, Hasher};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::SystemTime;
use tiny_http::{Header, Method, Request, Response, Server};

pub const SESSION_HEADER: &str = "Mcp-Session-Id";

fn new_session_id() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let mut out = String::new();
    for _ in 0..2 {
        // RandomState is seeded from OS randomness.
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u64(COUNTER.fetch_add(1, Ordering::Relaxed));
        h.write_u128(
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0),
        );
        out += &format!("{:016x}", h.finish());
    }
    out
}

fn header(name: &str, value: &str) -> Header {
    Header::from_bytes(name.as_bytes(), value.as_bytes()).expect("valid header")
}

fn get_header<'a>(req: &'a Request, name: &str) -> Option<&'a str> {
    req.headers()
        .iter()
        .find(|h| h.field.as_str().as_str().eq_ignore_ascii_case(name))
        .map(|h| h.value.as_str())
}

fn json_response(status: u16, body: &Value) -> Response<std::io::Cursor<Vec<u8>>> {
    Response::from_string(body.to_string())
        .with_status_code(status)
        .with_header(header("Content-Type", "application/json"))
}

/// Bind the server. Fails if the port is taken (usually: already running).
pub fn bind(port: u16) -> Result<Server, String> {
    Server::http(("127.0.0.1", port)).map_err(|e| e.to_string())
}

pub fn spawn(server: Server, shared: Arc<Shared>, port: u16) {
    let server = Arc::new(server);
    std::thread::Builder::new()
        .name("http".into())
        .spawn(move || {
            for req in server.incoming_requests() {
                let shared = shared.clone();
                // tool calls may block for a long time (request_control waits)
                std::thread::spawn(move || handle(req, &shared, port));
            }
        })
        .expect("spawn http thread");
}

fn is_loopback(host: &str) -> bool {
    host.eq_ignore_ascii_case("localhost") || host == "127.0.0.1" || host == "[::1]"
}

/// Splits `host[:port]`, where host may be a bracketed IPv6 address.
fn split_host_port(s: &str) -> (&str, Option<&str>) {
    match s.rfind(':') {
        Some(i) if !s[i..].contains(']') => (&s[..i], Some(&s[i + 1..])),
        _ => (s, None),
    }
}

/// The `Host` header must name this server exactly (defeats DNS rebinding).
fn host_allowed(host: Option<&str>, port: u16) -> bool {
    let Some(host) = host else { return true };
    let (name, p) = split_host_port(host);
    is_loopback(name) && p.and_then(|p| p.parse::<u16>().ok()) == Some(port)
}

/// Requests from web pages carry an `Origin`; only local pages are accepted.
/// The host is compared exactly, so `http://localhost.example.com` is refused.
fn origin_allowed(origin: Option<&str>) -> bool {
    let Some(origin) = origin else { return true };
    let Some(rest) = origin
        .get(..7)
        .filter(|scheme| scheme.eq_ignore_ascii_case("http://"))
        .map(|_| &origin[7..])
    else {
        return false;
    };
    let (name, p) = split_host_port(rest);
    is_loopback(name) && p.is_none_or(|p| p.parse::<u16>().is_ok())
}

/// A browser can only POST `application/json` cross-origin after a CORS
/// preflight, which this server never approves.
fn is_json(content_type: Option<&str>) -> bool {
    content_type.is_some_and(|ct| {
        ct.split(';')
            .next()
            .is_some_and(|t| t.trim().eq_ignore_ascii_case("application/json"))
    })
}

fn handle(mut req: Request, shared: &Shared, port: u16) {
    if !host_allowed(get_header(&req, "Host"), port) || !origin_allowed(get_header(&req, "Origin"))
    {
        let _ = req.respond(Response::from_string("forbidden").with_status_code(403));
        return;
    }
    let path = req.url().split('?').next().unwrap_or("").to_string();
    let method = req.method().clone();
    match (method, path.as_str()) {
        (Method::Get, "/status") => {
            let _ = req.respond(json_response(200, &status_json(shared)));
        }
        (Method::Post, "/mcp") => {
            if !is_json(get_header(&req, "Content-Type")) {
                let _ = req.respond(
                    Response::from_string("Content-Type must be application/json")
                        .with_status_code(415),
                );
                return;
            }
            let mut body = String::new();
            if req.as_reader().read_to_string(&mut body).is_err() {
                let _ = req.respond(Response::from_string("bad body").with_status_code(400));
                return;
            }
            let session = get_header(&req, SESSION_HEADER).map(str::to_string);
            let (status, out, new_session) = handle_mcp_post(shared, session, &body);
            let mut resp = match out {
                Some(v) => json_response(status, &v),
                None => Response::from_string("").with_status_code(status),
            };
            if let Some(s) = new_session {
                resp = resp.with_header(header(SESSION_HEADER, &s));
            }
            let _ = req.respond(resp);
        }
        (Method::Delete, "/mcp") => {
            if let Some(s) = get_header(&req, SESSION_HEADER).map(str::to_string) {
                shared.sessions.lock().unwrap().remove(&s);
                shared.with(|b, now| b.forget(&s, now));
            }
            let _ = req.respond(Response::from_string("").with_status_code(200));
        }
        (Method::Get, "/mcp") => {
            // We don't offer a server->client SSE stream.
            let _ = req.respond(Response::from_string("").with_status_code(405));
        }
        _ => {
            let _ = req.respond(Response::from_string("not found").with_status_code(404));
        }
    }
}

/// Returns (http status, body, newly created session id).
fn handle_mcp_post(
    shared: &Shared,
    session: Option<String>,
    body: &str,
) -> (u16, Option<Value>, Option<String>) {
    let msg: Value = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(e) => {
            return (
                400,
                Some(mcp::rpc_error(
                    &Value::Null,
                    -32700,
                    &format!("Parse error: {e}"),
                )),
                None,
            );
        }
    };
    if msg.is_array() {
        return (
            400,
            Some(mcp::rpc_error(
                &Value::Null,
                -32600,
                "JSON-RPC batches are not supported",
            )),
            None,
        );
    }

    if msg.get("method").and_then(Value::as_str) == Some("initialize") {
        let id = new_session_id();
        let client_name = msg
            .pointer("/params/clientInfo/title")
            .or_else(|| msg.pointer("/params/clientInfo/name"))
            .and_then(Value::as_str)
            .map(|n| mcp::clean_label(n, mcp::MAX_AGENT_CHARS))
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| "Unknown agent".into());
        shared.sessions.lock().unwrap().insert(
            id.clone(),
            Session {
                client_name,
                agent_name: None,
            },
        );
        let resp = mcp::handle(shared, &id, &msg);
        return (200, resp, Some(id));
    }

    let Some(session) = session else {
        let id = msg.get("id").cloned().unwrap_or(Value::Null);
        return (
            400,
            Some(mcp::rpc_error(
                &id,
                -32600,
                "Missing Mcp-Session-Id header; send initialize first",
            )),
            None,
        );
    };
    if !shared.sessions.lock().unwrap().contains_key(&session) {
        // Spec: unknown session -> 404, client must re-initialize.
        let id = msg.get("id").cloned().unwrap_or(Value::Null);
        return (
            404,
            Some(mcp::rpc_error(&id, -32001, "Session not found")),
            None,
        );
    }
    match mcp::handle(shared, &session, &msg) {
        Some(v) => (200, Some(v), None),
        None => (202, None, None),
    }
}

fn status_json(shared: &Shared) -> Value {
    let hardware = shared.hardware_detection();
    let (hw_total, injected_total) = crate::hook::totals();
    shared.with(|b, now| {
        json!({
            "holder": b.holder().map(|l| json!({
                "agent": l.agent, "task": l.task,
                "remaining_secs": l.expires_at.saturating_duration_since(now).as_secs(),
            })),
            "queue": b.queue().iter().map(|r| json!({ "agent": r.agent, "task": r.task })).collect::<Vec<_>>(),
            "auto_grant_in_secs": b.auto_grant_in(now).map(|d| d.as_secs()),
            "cursor": b.cursor(),
            "detection": if hardware { "hardware" } else { "movement" },
            "input_events": { "physical": hw_total, "software": injected_total },
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_must_be_this_server() {
        for ok in [
            "127.0.0.1:47321",
            "localhost:47321",
            "LOCALHOST:47321",
            "[::1]:47321",
        ] {
            assert!(host_allowed(Some(ok), 47321), "{ok}");
        }
        assert!(host_allowed(None, 47321));
        for bad in [
            "127.0.0.1:1",
            "127.0.0.1",
            "evil.com:47321",
            "localhost.evil.com:47321",
            "127.0.0.1.nip.io:47321",
        ] {
            assert!(!host_allowed(Some(bad), 47321), "{bad}");
        }
    }

    #[test]
    fn origin_must_be_a_local_page() {
        for ok in [
            "http://localhost",
            "http://127.0.0.1:3000",
            "http://[::1]:8080",
            "HTTP://LocalHost",
        ] {
            assert!(origin_allowed(Some(ok)), "{ok}");
        }
        assert!(origin_allowed(None));
        for bad in [
            "http://localhost.evil.com",
            "http://127.0.0.1.nip.io",
            "http://localhost:80@evil.com",
            "http://localhost:x",
            "https://evil.com",
            "null",
            "",
        ] {
            assert!(!origin_allowed(Some(bad)), "{bad}");
        }
    }

    #[test]
    fn only_json_posts() {
        assert!(is_json(Some("application/json")));
        assert!(is_json(Some("Application/JSON; charset=utf-8")));
        for bad in [
            None,
            Some("text/plain"),
            Some("application/x-www-form-urlencoded"),
            Some(""),
        ] {
            assert!(!is_json(bad), "{bad:?}");
        }
    }
}
