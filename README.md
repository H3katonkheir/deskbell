# Deskbell 🔔

A tiny always-on-top **desk bell** for your desktop that AI agents ring (through
[MCP](https://modelcontextprotocol.io)) when they want to use **your mouse and keyboard**, e.g.
for computer-use / GUI automation tasks while you're also working.

- When an agent rings, the bell shakes and a **speech bubble** shows *which agent* wants
  control and *for what task*.
- **Click the bell** to allow; **right-click** to deny.
- If you leave the computer alone for **30 s** (configurable) while it rings, access is granted
  automatically.
- One agent at a time; everyone else waits in a **queue** (shown as a `+N` badge).
- Access is a **1-minute lease** (configurable) that the agent can renew. A ring around the
  bell shows the time left.
- **Move the mouse (or type, on Windows) and you instantly take control back.** The agent can
  ask again and goes to the **front** of the queue.
- Hover the bell and a menu fans out down its right side: ✕ exit, ⚙ settings, ✥ move the
  widget, and the 🔥 **auto-fire** toggle (lit when on, grey when off). While it's on,
  requests are granted without asking (as soon as you've left the mouse alone for 2 s);
  agents still take turns and moving the mouse still takes control back.
  Software-generated clicks can't toggle it, and it's always off when deskbell starts.
- When nothing has happened for **30 s** (configurable, can be turned off) the bell fades to a
  plain outline; hovering it or a new request brings it back.

Written in Rust with [egui/eframe](https://github.com/emilk/egui) (transparent, undecorated,
always-on-top window) and [enigo](https://github.com/enigo-rs/enigo) (input). Builds for
Windows, macOS and Linux (X11) from one ~8 MB binary. Windows gets the most complete
experience (see [How the bell knows it's you](#how-the-bell-knows-its-you)).

## Install

deskbell is a single file: no installer and no admin rights needed.

1. **Download** the binary for your platform from the
   [latest release](https://github.com/H3katonkheir/deskbell/releases/latest):
   `deskbell-windows-x64.exe`, `deskbell-macos-arm64` (Apple silicon), `deskbell-macos-x64`
   (Intel Mac) or `deskbell-linux-x64` (glibc 2.35 or newer, e.g. Ubuntu 22.04+ or Debian 12+).
2. **Run it.**
   - **Windows:** double-click it. SmartScreen may warn about an unrecognised app: *More info →
     Run anyway*.
   - **macOS / Linux:** in a terminal, make it executable and run it. On macOS, also clear the
     download quarantine first, or Gatekeeper refuses to open it:

     ```sh
     chmod +x deskbell-macos-arm64                          # or deskbell-macos-x64 / deskbell-linux-x64
     xattr -d com.apple.quarantine deskbell-macos-arm64     # macOS only
     ./deskbell-macos-arm64
     ```
3. **Click Install.** On first run the bell opens its settings. *Install & startup* → **Install**
   copies deskbell to a fixed place and restarts it from there:
   - Windows: `%LOCALAPPDATA%\Programs\deskbell\deskbell.exe`, with a Start menu shortcut
   - macOS / Linux: `~/.local/bin/deskbell` (plus an app launcher entry on Linux)

   This matters because agent configs store the full path to the binary: install first, then
   connect your agents, and you can delete the downloaded file. (From a terminal,
   `deskbell install` does the same.)
4. **Optionally** switch on *Start deskbell when I log in* in the same card.
5. **Connect your agents** from the settings window (*Connect your agents*), or see
   [below](#connect-an-agent).

The bell sits in the bottom-right corner; hover it for the menu (⚙ opens settings again).

**Updates.** An installed copy checks for a new release once a day (Settings → *Updates*, or
*Check now*) and downloads it in the background; a dot on the ⚙ icon shows when one is ready.
Installing is always your click on *Restart to update*: the binary is replaced in place and the
bell restarts. Every update must carry a signature from the maintainer's release key, so a
compromised download or repository alone can't push code to you.

**Uninstall:** run `deskbell uninstall` (Windows:
`%LOCALAPPDATA%\Programs\deskbell\deskbell.exe uninstall`). It removes the installed binary, the
shortcut and the login entry, and keeps your settings (Settings → *Config file* shows where).

### Build from source

```sh
cargo build --release
# binary: target/release/deskbell(.exe)
```

Linux build dependencies (Debian/Ubuntu):
`sudo apt install libxcb-render0-dev libxcb-shape0-dev libxcb-xfixes0-dev libxkbcommon-dev`

Builds running from a cargo `target` folder don't offer to install or update themselves. Every
CI run also attaches the four binaries as artifacts.

## Connect an agent

Install and start the bell first ([above](#install)), then connect your agent. Wherever a config
needs the path to the binary, use the installed one: `%LOCALAPPDATA%\Programs\deskbell\deskbell.exe`
on Windows, `~/.local/bin/deskbell` (written out in full, e.g. `/home/you/.local/bin/deskbell`)
on macOS and Linux.

### Claude Code plugin (recommended)

The repo is also a Claude Code plugin marketplace. The plugin registers the MCP server **and**
installs the `deskbell` skill, which teaches the agent the etiquette: prepare before ringing,
renew, release, type only into a field it just clicked, stop immediately when it loses control,
and never click the bell.

```text
/plugin marketplace add H3katonkheir/deskbell
/plugin install deskbell@deskbell
```

On install, Claude Code asks for the full path to the `deskbell` binary.

### Other MCP clients

The settings window (⚙ → *Connect your agents*) has a tab per client with these filled in for
your machine: copy buttons, one-click **Add to VS Code** / **Add to Cursor** links, and buttons
that open each client's config file. Replace `/path/to/deskbell` below with the full path to
the installed binary.

Agents other than Claude Code don't get the skill, but the server's MCP `instructions` carry the
same rules in short form. You can also copy [`skills/deskbell/SKILL.md`](skills/deskbell/SKILL.md)
into `~/.claude/skills/deskbell/` to use the skill without the plugin.

**Claude Code** (server only, without the plugin):

```sh
claude mcp add deskbell -- /path/to/deskbell mcp
# optional: a fixed name for the bubble
claude mcp add deskbell -- /path/to/deskbell mcp --agent "Claude (repo X)"
```

**Codex CLI**:

```sh
codex mcp add deskbell -- /path/to/deskbell mcp
```

or in `~/.codex/config.toml`:

```toml
[mcp_servers.deskbell]
command = '/path/to/deskbell'
args = ["mcp"]
```

**Gemini CLI**:

```sh
gemini mcp add -s user deskbell /path/to/deskbell mcp
```

**VS Code** (`mcp.json`, or use the *Add to VS Code* button):

```json
{ "servers": { "deskbell": { "type": "stdio", "command": "/path/to/deskbell", "args": ["mcp"] } } }
```

**Claude Desktop** (`claude_desktop_config.json`), **Cursor** (`~/.cursor/mcp.json`, or the
*Add to Cursor* button), **Windsurf** (`~/.codeium/windsurf/mcp_config.json`) and most other
clients:

```json
{ "mcpServers": { "deskbell": { "command": "/path/to/deskbell", "args": ["mcp"] } } }
```

The stdio server starts the bell if it isn't running.

**Streamable HTTP** (any client that supports it; the bell must already be running):

```sh
claude mcp add --transport http deskbell http://127.0.0.1:47321/mcp
```

### Tools

| Tool | Lease needed | What it does |
|---|---|---|
| `request_control(task, agent_name?, wait_seconds=45)` | | Ring the bell. Blocks up to `wait_seconds`; returns `GRANTED`, `WAITING` (call again) or `DENIED`. |
| `renew_control()` | ✓ | Extend the lease by a full period. |
| `release_control()` | | Give control back / leave the queue. |
| `control_status()` | | Holding? Queue position? Time left? Which input tools are allowed here? |
| `get_screen_info()` | | Main display size and cursor position. |
| `mouse_move(x, y)` | ✓ | Absolute move. |
| `mouse_click(x?, y?, button=left, count=1)` | ✓ | Click / double-click (optionally move first). |
| `mouse_drag(from_x, from_y, to_x, to_y, button=left)` | ✓ | Press, move, release. |
| `mouse_scroll(amount, horizontal=false, x?, y?)` | ✓ | Wheel notches. |
| `type_text(text)` | ✓ | Type Unicode text into the focused window (`\n` = Enter). |
| `press_keys(keys, repeat=1)` | ✓ | A key or shortcut: `enter`, `tab`, `ctrl+s`, `ctrl+shift+t`, `alt+tab`, `f5`, … |

The server also sends MCP `instructions` telling the agent the etiquette and which input tools
it may use on this machine.

Coordinates are physical pixels on Windows/Linux and logical points on macOS, with the origin
at the top-left of the main display.

## How it works

```
 agent A ─stdio─► deskbell mcp ─┐
 agent B ─stdio─► deskbell mcp ─┤ HTTP (127.0.0.1:47321/mcp)
 agent C ─── Streamable HTTP ───┤
                                ▼
                 ┌──────────── deskbell (widget) ─────────────┐
                 │ broker: queue, lease, idle timer           │
                 │ monitor: cursor @25 Hz + input hooks (Win) │
                 │ input: enigo mouse + keyboard actions      │
                 │ ui: egui transparent bell window           │
                 └────────────────────────────────────────────┘
```

* The widget is the single source of truth (one process, one queue). Each agent's MCP client
  spawns its own lightweight `deskbell mcp` bridge, which forwards JSON-RPC to the widget;
  the bridge launches the widget if needed and transparently re-initializes if the widget restarts.
* Each MCP session is one queue entry; the bubble shows `agent_name` or the client's
  `clientInfo.name`.
* Transparent parts of the window are click-through: the widget toggles mouse passthrough
  based on whether the cursor is over the bell or its menu.
* `GET http://127.0.0.1:47321/status` returns the current state as JSON (handy for debugging).
* Settings live in the platform config dir (`%APPDATA%\deskbell\...`,
  `~/Library/Application Support/dev.deskbell.deskbell/`, `~/.config/deskbell/`).

### How the bell knows it's you

Settings → *How the bell knows it's you*:

* **Only my physical mouse and keyboard count** (Windows, default). Low-level hooks
  (`WH_MOUSE_LL`, `WH_KEYBOARD_LL`) see every input event along with the "injected" flag
  Windows sets on software-generated input. Only physical input counts as you: moving the
  mouse more than the threshold, pressing a button or the wheel, or (optional, on by default)
  pressing a key. As a result:
  * agents may use **their own computer-use tools** while they hold the lease;
  * your typing takes control back, and it also stops the bell auto-allowing while you type;
  * **software-generated clicks on the bell are ignored**, so an agent can't approve itself or
    close the bell.

  Cursor jumps that come with no input event at all (`SetCursorPos`, or input over elevated
  windows that the hook can't see) count as you by default; you can switch that off for agents
  whose tools use `SetCursorPos`.
* **Any movement** (the only mode on macOS/Linux for now). The cursor moving more than the
  threshold from where it was after the agent's last action counts as you. Only deskbell's own
  tools are excluded, so agents must use them. Typing can't be detected in this mode.

## Things to be aware of

* **It's a courtesy protocol, not a security boundary.** An agent that has its own input
  tools can still ignore the bell entirely; the bell only makes polite agents polite. On
  Windows (physical-input mode) an agent can at least no longer click the bell to approve
  itself. For real enforcement, give agents input access **only** through deskbell's tools.
* **Software that generates input for you.** Mouse Without Borders, Synergy/Barrier/
  Input Leap, remote-control tools (AnyDesk, TeamViewer), on-screen keyboards, and some voice
  or eye-tracking control inject input that Windows flags exactly like an agent's. With those,
  the bell can't see you (no revocation, and it auto-allows while you're "idle") and ignores
  your clicks on it. Switch to *Any movement* in the settings if you use one. Mouse/keyboard
  vendor utilities that remap buttons or run macros can have a similar effect.
* **Typing goes to the focused window.** Agents should click into the target field right
  before typing; the skill and server instructions tell them so. If focus moves between the
  click and the typing, keystrokes land elsewhere.
* **macOS/Linux** don't have physical-input detection yet (the equivalents are CGEvent source
  state on macOS and XInput2 device IDs on X11), so there agents must use deskbell's tools,
  and your typing doesn't take control back.
* **Focus stealing.** Clicking the bell activates the widget window; the agent's first click
  will re-focus the target app.
* **Screens and scaling.** Agents usually work from screenshots, which may be scaled. Use
  `get_screen_info` to calibrate. Multi-monitor setups with mixed DPI are only lightly tested.
* **macOS** needs *Accessibility* permission (System Settings → Privacy & Security →
  Accessibility) for the injected input. The bell may not show over full-screen apps or on other
  Spaces.
* **Linux:** X11 sessions work. On native **Wayland**, apps can't read the global cursor
  position or place always-on-top windows, so the widget forces XWayland; even then cursor
  tracking only works over X11 windows. Treat Wayland as unsupported for now.
* **Locked screen / screensaver / UAC prompts:** injected input won't reach secure desktops.
* **Localhost API:** the HTTP endpoint only binds to `127.0.0.1` and rejects requests from web
  pages (exact Host/Origin checks against DNS rebinding, and JSON-only POSTs so browsers can't
  send them without asking), but any local process can talk to it.
* **Startup:** switch on *Start deskbell when I log in* in settings if you want the bell always
  present. The stdio bridge also starts it on demand.

Found a security problem? Please report it privately, as described in [SECURITY.md](SECURITY.md).

## Development

```sh
cargo test      # state machine, input classification and key parsing tests
cargo clippy
cargo run       # start the widget (debug build has a console window on Windows)
```

Source layout: `broker.rs` (pure state machine), `state.rs` (shared state + waiting),
`input.rs` (cursor monitor + enigo actions + key parsing), `hook.rs` (Windows physical-vs-injected
input detection), `mcp.rs` (protocol + tools), `http.rs` (Streamable HTTP endpoint),
`bridge.rs` (stdio ↔ HTTP), `ui.rs` (widget + settings), `clients.rs` (MCP client install
shortcuts), `update.rs` + `manifest.rs` (signed self-update), `icon.rs`, `config.rs`. The Claude
Code plugin is `.claude-plugin/` + `skills/deskbell/SKILL.md`; check it with
`claude plugin validate .`.

### Releasing

1. Bump `version` in `Cargo.toml` and `.claude-plugin/plugin.json`, commit, then tag and push:
   `git tag v1.2.3 && git push --tags`. CI builds the four binaries and creates the GitHub
   release with generated notes.
2. Sign it: `.\scripts\sign-release.ps1 v1.2.3`. This takes the binaries from the tag's CI run,
   checks that the run built the commit your local tag points to and that the release's files
   are byte-for-byte the same, then signs an `update-<platform>.json` for each and uploads them
   to the release. Installed copies only see a release once it's signed.

Updates are signed with the private key in `%USERPROFILE%\.deskbell\update-signing.key`
(`~/.deskbell/` elsewhere); the public half is built in from `assets/update-public-key.txt`.
**Back the private key up offline and never put it in the repo or CI.** If it's lost, installed
copies can no longer update and everyone has to download the next version by hand. A new
checkout creates one with `cargo run --example release_signing -- keygen` (it refuses to
replace an existing key).

To test an update, point a copy (outside the `target` folder) at another source:
`DESKBELL_UPDATE_BASE=http://127.0.0.1:8000` serves `update-<platform>.json` and the binary from
a local folder. Signatures are still required, so this can't be used to bypass them.
