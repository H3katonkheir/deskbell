use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::Duration;

pub const DEFAULT_PORT: u16 = 47_321;

/// How the widget decides that *the user* (not an agent) moved the mouse.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Detection {
    /// Only physical mouse input counts (Windows). Software-injected input,
    /// e.g. an agent's own computer-use tool, is ignored, and synthetic
    /// clicks on the bell are rejected.
    Hardware,
    /// Any cursor movement not made by this server's own mouse_* tools counts.
    Movement,
}

/// Colour theme of the settings window.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Theme {
    /// Follow the operating system.
    #[default]
    System,
    Light,
    Dark,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Config {
    /// Local port of the HTTP / MCP endpoint (restart required).
    pub port: u16,
    /// Seconds the cursor must stay still while the bell rings before access
    /// is granted automatically. 0 disables auto-granting.
    pub auto_grant_idle_secs: u64,
    /// Length of a lease in seconds (agents may renew).
    pub lease_secs: u64,
    /// Cursor movement (in pixels) that counts as "the user moved the mouse".
    pub move_threshold_px: i32,
    /// Grant requests without asking ("auto-fire"); agents still take turns
    /// and moving the mouse still takes control back. Deliberately not saved:
    /// every start begins with auto-fire off.
    #[serde(skip)]
    pub auto_fire: bool,
    /// Show the agent/task speech bubble.
    pub show_bubble: bool,
    pub detection: Detection,
    /// Hardware mode: cursor jumps that produce no input event at all
    /// (`SetCursorPos`, or input the hook can't see, e.g. over elevated
    /// windows) are attributed to the user. Safer; turn off if your agent's
    /// tools move the cursor with `SetCursorPos` (e.g. pyautogui).
    pub unexplained_moves_are_user: bool,
    /// Hardware mode: physical key presses also take control back.
    pub keyboard_takes_back: bool,
    pub theme: Theme,
    /// Fade the bell to a plain outline when nothing has happened for a while.
    pub simplify_when_idle: bool,
    /// Seconds without activity or hovering before the bell is simplified.
    pub simplify_after_secs: u64,
    /// Look for a new version once a day (installing is always a click).
    pub auto_update: bool,
    /// Last window position (logical points), restored on start.
    pub window_pos: Option<[f32; 2]>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            port: DEFAULT_PORT,
            auto_grant_idle_secs: 30,
            lease_secs: 60,
            move_threshold_px: 8,
            auto_fire: false,
            show_bubble: true,
            detection: Detection::Hardware,
            unexplained_moves_are_user: true,
            keyboard_takes_back: true,
            theme: Theme::System,
            simplify_when_idle: true,
            simplify_after_secs: 30,
            auto_update: true,
            window_pos: None,
        }
    }
}

impl Config {
    pub fn path() -> Option<PathBuf> {
        directories::ProjectDirs::from("dev", "deskbell", "deskbell")
            .map(|d| d.config_dir().join("config.json"))
    }

    pub fn load() -> Self {
        Self::path()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) -> std::io::Result<()> {
        let Some(path) = Self::path() else {
            return Ok(());
        };
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(path, serde_json::to_string_pretty(self)?)
    }

    pub fn broker_settings(&self) -> crate::broker::Settings {
        crate::broker::Settings {
            idle_grant: (self.auto_grant_idle_secs > 0)
                .then(|| Duration::from_secs(self.auto_grant_idle_secs)),
            lease: Duration::from_secs(self.lease_secs.max(5)),
            move_threshold: self.move_threshold_px.max(1),
            auto_fire: self.auto_fire,
        }
    }
}
