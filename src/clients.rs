//! How to register deskbell with common MCP clients: commands to copy,
//! one-click install links and config snippets, shown in the settings window.

use std::path::PathBuf;

pub enum Item {
    /// A value to copy, with a caption.
    Copy {
        label: &'static str,
        value: String,
    },
    /// A button that opens a link (a client's own install handler).
    Link {
        label: &'static str,
        url: String,
    },
    /// A button that shows a client's config file (or its folder).
    Reveal {
        label: &'static str,
        path: PathBuf,
    },
    Note(&'static str),
}

pub struct Client {
    pub name: &'static str,
    pub items: Vec<Item>,
}

fn home() -> Option<PathBuf> {
    directories::BaseDirs::new().map(|d| d.home_dir().to_path_buf())
}

/// `%APPDATA%` on Windows, `~/Library/Application Support` on macOS, `~/.config` on Linux.
fn app_config_dir() -> Option<PathBuf> {
    directories::BaseDirs::new().map(|d| d.config_dir().to_path_buf())
}

/// The usual `{ "mcpServers": { "deskbell": … } }` block.
fn mcp_servers_json(exe: &str) -> String {
    let json =
        serde_json::json!({ "mcpServers": { "deskbell": { "command": exe, "args": ["mcp"] } } });
    serde_json::to_string_pretty(&json).unwrap_or_default()
}

pub fn all(exe: &str, url: &str) -> Vec<Client> {
    use Item::*;
    let stdio = serde_json::json!({ "command": exe, "args": ["mcp"] });
    let mut clients = vec![
        Client {
            name: "Claude Code",
            items: vec![
                Note("The plugin adds the server and a skill that teaches the etiquette (recommended)."),
                Copy {
                    label: "1. Add the marketplace",
                    value: "/plugin marketplace add H3katonkheir/deskbell".into(),
                },
                Copy {
                    label: "2. Install the plugin (it asks for the path below)",
                    value: "/plugin install deskbell@deskbell".into(),
                },
                Copy {
                    label: "Path to deskbell",
                    value: exe.into(),
                },
                Copy {
                    label: "Or only the server (stdio)",
                    value: format!("claude mcp add deskbell -- \"{exe}\" mcp"),
                },
                Copy {
                    label: "Or over HTTP (the bell must be running)",
                    value: format!("claude mcp add --transport http deskbell {url}"),
                },
            ],
        },
        Client {
            name: "Claude Desktop",
            items: vec![
                Note("Add this to claude_desktop_config.json, then restart Claude Desktop."),
                Copy {
                    label: "claude_desktop_config.json",
                    value: mcp_servers_json(exe),
                },
            ],
        },
        Client {
            name: "Codex",
            items: vec![
                Copy {
                    label: "Codex CLI",
                    value: format!("codex mcp add deskbell -- \"{exe}\" mcp"),
                },
                Copy {
                    label: "Or add to ~/.codex/config.toml",
                    value: format!("[mcp_servers.deskbell]\ncommand = '{exe}'\nargs = [\"mcp\"]"),
                },
            ],
        },
        Client {
            name: "Gemini CLI",
            items: vec![Copy {
                label: "Gemini CLI (user scope)",
                value: format!("gemini mcp add -s user deskbell \"{exe}\" mcp"),
            }],
        },
        Client {
            name: "VS Code",
            items: vec![
                Link {
                    label: "Add to VS Code",
                    url: format!(
                        "vscode:mcp/install?{}",
                        percent_encode(
                            &serde_json::json!({ "name": "deskbell", "type": "stdio", "command": exe, "args": ["mcp"] })
                                .to_string()
                        )
                    ),
                },
                Copy {
                    label: "Or add to mcp.json",
                    value: serde_json::to_string_pretty(&serde_json::json!({
                        "servers": { "deskbell": { "type": "stdio", "command": exe, "args": ["mcp"] } }
                    }))
                    .unwrap_or_default(),
                },
            ],
        },
        Client {
            name: "Cursor",
            items: vec![
                Link {
                    label: "Add to Cursor",
                    url: format!(
                        "cursor://anysphere.cursor-deeplink/mcp/install?name=deskbell&config={}",
                        percent_encode(&base64(stdio.to_string().as_bytes()))
                    ),
                },
                Copy {
                    label: "Or add to ~/.cursor/mcp.json",
                    value: mcp_servers_json(exe),
                },
            ],
        },
        Client {
            name: "Windsurf",
            items: vec![Copy {
                label: "Add to ~/.codeium/windsurf/mcp_config.json",
                value: mcp_servers_json(exe),
            }],
        },
        Client {
            name: "Other",
            items: vec![
                Copy {
                    label: "Generic mcpServers JSON (stdio)",
                    value: mcp_servers_json(exe),
                },
                Copy {
                    label: "Streamable HTTP endpoint (the bell must be running)",
                    value: url.into(),
                },
            ],
        },
    ];

    // Buttons to the clients' config files, where we know them.
    let add_reveal = |clients: &mut Vec<Client>, name: &str, path: Option<PathBuf>| {
        if let (Some(c), Some(path)) = (clients.iter_mut().find(|c| c.name == name), path) {
            c.items.push(Reveal {
                label: "Show config file",
                path,
            });
        }
    };
    add_reveal(
        &mut clients,
        "Claude Desktop",
        app_config_dir().map(|d| d.join("Claude").join("claude_desktop_config.json")),
    );
    add_reveal(
        &mut clients,
        "Codex",
        home().map(|h| h.join(".codex").join("config.toml")),
    );
    add_reveal(
        &mut clients,
        "Cursor",
        home().map(|h| h.join(".cursor").join("mcp.json")),
    );
    add_reveal(
        &mut clients,
        "Windsurf",
        home().map(|h| h.join(".codeium").join("windsurf").join("mcp_config.json")),
    );
    clients
}

/// Percent-encode everything but RFC 3986 unreserved characters.
pub fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Standard base64 with padding.
pub fn base64(bytes: &[u8]) -> String {
    const ABC: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, &b)| n | (b as u32) << (16 - 8 * i));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ABC[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// Open a URL (including app links like `vscode:`) with the system handler.
pub fn open_url(url: &str) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        use windows_sys::Win32::UI::Shell::ShellExecuteW;
        use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
        let wide = |s: &str| s.encode_utf16().chain(Some(0)).collect::<Vec<u16>>();
        let (op, file) = (wide("open"), wide(url));
        let r = unsafe {
            ShellExecuteW(
                std::ptr::null_mut(),
                op.as_ptr(),
                file.as_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                SW_SHOWNORMAL,
            )
        };
        // Values above 32 mean success.
        if r as isize > 32 {
            Ok(())
        } else {
            Err(std::io::Error::other("no app is registered for this link"))
        }
    }
    #[cfg(not(windows))]
    {
        let cmd = if cfg!(target_os = "macos") {
            "open"
        } else {
            "xdg-open"
        };
        std::process::Command::new(cmd).arg(url).spawn().map(reap)
    }
}

/// Waits for a launched helper (file manager, `open`, `xdg-open`) in the
/// background, so it doesn't linger as a zombie process on Linux and macOS.
pub fn reap(mut child: std::process::Child) {
    let _ = std::thread::Builder::new()
        .name("reap".into())
        .spawn(move || child.wait());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_known_values() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
        assert_eq!(base64(&[0xfb, 0xff]), "+/8=");
    }

    #[test]
    fn percent_encoding_keeps_only_unreserved() {
        assert_eq!(percent_encode("a-Z_0.~"), "a-Z_0.~");
        assert_eq!(
            percent_encode(r#"{"a": "C:\x"}"#),
            "%7B%22a%22%3A%20%22C%3A%5Cx%22%7D"
        );
        assert_eq!(percent_encode("+/="), "%2B%2F%3D");
    }

    #[test]
    fn every_client_has_something_to_show() {
        for c in all(r"C:\deskbell.exe", "http://127.0.0.1:1/mcp") {
            assert!(!c.items.is_empty(), "{}", c.name);
        }
    }

    #[test]
    fn windows_paths_survive_in_json_and_toml() {
        let clients = all(r"C:\Program Files\deskbell.exe", "u");
        let codex = clients.iter().find(|c| c.name == "Codex").unwrap();
        let Item::Copy { value, .. } = &codex.items[1] else {
            panic!()
        };
        // TOML literal string: backslashes stay as they are.
        assert!(value.contains(r"command = 'C:\Program Files\deskbell.exe'"));
        let other = clients.iter().find(|c| c.name == "Other").unwrap();
        let Item::Copy { value, .. } = &other.items[0] else {
            panic!()
        };
        let parsed: serde_json::Value = serde_json::from_str(value).unwrap();
        assert_eq!(
            parsed["mcpServers"]["deskbell"]["command"],
            r"C:\Program Files\deskbell.exe"
        );
    }
}
