// No console window for the widget in release builds on Windows. (stdio still
// works for `deskbell mcp` when the MCP client provides pipes.)
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod autostart;
mod bridge;
mod broker;
mod clients;
mod config;
mod hook;
mod http;
mod icon;
mod input;
mod install;
mod manifest;
mod mcp;
mod state;
mod ui;
mod update;
#[cfg(windows)]
mod winframe;

use config::Config;
use std::sync::Arc;

const HELP: &str = "\
deskbell - a desk bell AI agents ring (via MCP) to ask for your mouse and keyboard

USAGE:
    deskbell                 Start the bell widget (also serves MCP over HTTP)
    deskbell mcp [OPTIONS]   Run a stdio MCP server that talks to the widget
                                  (starts the widget if it isn't running)
    deskbell install         Copy deskbell to a fixed per-user location
                                  (Windows: %LOCALAPPDATA%\\Programs\\deskbell, with a
                                  Start menu shortcut; elsewhere: ~/.local/bin)
    deskbell uninstall       Remove the installed copy, shortcut and login entry

MCP OPTIONS:
    --agent <NAME>     Name shown in the bell's speech bubble (default: the MCP client's name)
    --port <PORT>      Port of the widget (default: from config, 47321)
    --no-autostart     Don't launch the widget if it isn't running

The widget's Streamable HTTP endpoint is http://127.0.0.1:<port>/mcp
";

/// Windows release builds are GUI apps with no console; when run from a
/// terminal, attach to it so commands like `--help` can print.
fn attach_terminal() {
    #[cfg(all(windows, not(debug_assertions)))]
    unsafe {
        use windows_sys::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole};
        AttachConsole(ATTACH_PARENT_PROCESS);
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let first_run = Config::path().is_some_and(|p| !p.exists());
    let config = Config::load();

    match args.first().map(String::as_str) {
        Some("-h" | "--help" | "help") => {
            attach_terminal();
            print!("{HELP}");
        }
        Some("-V" | "--version") => {
            attach_terminal();
            println!("deskbell {}", env!("CARGO_PKG_VERSION"));
        }
        Some("install") => {
            attach_terminal();
            match install::install() {
                Ok(path) => {
                    let path = autostart::plain(&path);
                    println!("Installed to {path}");
                    println!("Start it with: \"{path}\"");
                    println!(
                        "Use this path when you connect agents (the settings window has the commands)."
                    );
                }
                Err(e) => {
                    eprintln!("deskbell: {e}");
                    std::process::exit(1);
                }
            }
        }
        Some("uninstall") => {
            attach_terminal();
            let notes = install::uninstall();
            if notes.is_empty() {
                println!(
                    "Uninstalled. Your settings are kept in {}",
                    Config::path()
                        .map(|p| p.display().to_string())
                        .unwrap_or_default()
                );
            }
            for n in &notes {
                eprintln!("deskbell: {n}");
            }
        }
        Some("mcp") => {
            let mut port = config.port;
            let mut agent = None;
            let mut autostart = true;
            let mut it = args[1..].iter();
            while let Some(a) = it.next() {
                match a.as_str() {
                    "--agent" => agent = it.next().cloned(),
                    "--port" => port = it.next().and_then(|p| p.parse().ok()).unwrap_or(port),
                    "--no-autostart" => autostart = false,
                    other => eprintln!("deskbell: ignoring unknown argument {other}"),
                }
            }
            bridge::run(port, agent, autostart);
        }
        None | Some("widget") => run_widget(config, first_run),
        Some(other) => {
            attach_terminal();
            eprintln!("deskbell: unknown command '{other}'\n\n{HELP}");
            std::process::exit(2);
        }
    }
}

fn run_widget(config: Config, first_run: bool) {
    let port = config.port;
    // After an update the new bell starts while the old one is still closing.
    let wait_for_port = std::env::var_os(update::RESTART_ENV).is_some();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let server = loop {
        match http::bind(port) {
            Ok(s) => break s,
            Err(_) if wait_for_port && std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(200));
            }
            Err(e) => {
                eprintln!(
                    "deskbell: cannot listen on 127.0.0.1:{port} ({e}). Is the bell already running?"
                );
                std::process::exit(1);
            }
        }
    };
    update::clean_up_old_binaries();
    if first_run {
        // Remember that settings were shown once.
        let _ = config.save();
    }
    let shared = Arc::new(state::Shared::new(config));
    http::spawn(server, shared.clone(), port);
    // First run opens settings: how to install and connect agents.
    if let Err(e) = ui::run(shared, first_run) {
        eprintln!("deskbell: {e}");
        std::process::exit(1);
    }
}
