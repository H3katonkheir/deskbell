//! Installing to a fixed, per-user location (no admin rights, no installer).
//!
//! MCP client configs store the full path of the binary, so a copy that lives
//! in Downloads breaks every agent setup once it's moved or re-downloaded. This
//! copies it to a stable home that self-update can still write to:
//!
//! * Windows: `%LOCALAPPDATA%\Programs\deskbell\deskbell.exe`, plus a Start menu shortcut.
//! * macOS / Linux: `~/.local/bin/deskbell` (plus an app-launcher entry on Linux).

use std::fs;
use std::path::{Path, PathBuf};

use crate::autostart::{self, plain};

const BINARY: &str = if cfg!(windows) {
    "deskbell.exe"
} else {
    "deskbell"
};

pub fn install_path() -> Option<PathBuf> {
    let dirs = directories::BaseDirs::new()?;
    Some(if cfg!(windows) {
        dirs.data_local_dir()
            .join("Programs")
            .join("deskbell")
            .join(BINARY)
    } else {
        dirs.home_dir().join(".local").join("bin").join(BINARY)
    })
}

fn same_file(a: &Path, b: &Path) -> bool {
    match (fs::canonicalize(a), fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Status {
    /// Running from the install location.
    Installed(PathBuf),
    /// Running from somewhere else; `target` is where it would go.
    Elsewhere {
        current: PathBuf,
        target: PathBuf,
    },
    /// A cargo build: installing a dev build by accident helps nobody.
    DevBuild,
    Unknown,
}

pub fn status() -> Status {
    let (Ok(current), Some(target)) = (std::env::current_exe(), install_path()) else {
        return Status::Unknown;
    };
    if same_file(&current, &target) {
        Status::Installed(target)
    } else if crate::update::is_dev_build(&current) {
        Status::DevBuild
    } else {
        Status::Elsewhere { current, target }
    }
}

/// Copies the running binary to the install location and adds a launcher.
/// Returns where it was installed.
pub fn install() -> Result<PathBuf, String> {
    let current = std::env::current_exe().map_err(|e| format!("Can't locate deskbell: {e}"))?;
    let target = install_path().ok_or("Can't find the home folder")?;
    if same_file(&current, &target) {
        return Ok(target);
    }
    let dir = target.parent().ok_or("Bad install path")?;
    fs::create_dir_all(dir).map_err(|e| format!("Could not create {}: {e}", dir.display()))?;

    // Copy next to the target first, then swap it in.
    let staged = target.with_extension("installing");
    fs::copy(&current, &staged).map_err(|e| format!("Could not copy deskbell: {e}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&staged, fs::Permissions::from_mode(0o755))
            .map_err(|e| e.to_string())?;
    }
    if target.exists() {
        // An older copy may still be running (e.g. an agent's `deskbell mcp`):
        // on Windows that can be renamed but not replaced. Cleaned up on start.
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        let old = dir.join(format!("{BINARY}.old-{stamp}"));
        fs::rename(&target, &old)
            .map_err(|e| format!("Could not move the existing copy aside: {e}"))?;
    }
    fs::rename(&staged, &target).map_err(|e| format!("Could not install: {e}"))?;

    add_launcher(&target)?;
    autostart::retarget(&target)?;
    Ok(target)
}

/// Starts the installed bell, which waits for this one to let go of the port.
pub fn relaunch(target: &Path) -> Result<(), String> {
    std::process::Command::new(target)
        .env(crate::update::RESTART_ENV, "1")
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("Installed, but could not start it: {e}"))
}

/// Removes the login entry, launchers and the installed binary. Returns notes
/// for anything left behind.
pub fn uninstall() -> Vec<String> {
    let mut notes = Vec::new();
    if let Err(e) = autostart::disable() {
        notes.push(e);
    }
    if let Some(launcher) = launcher_path()
        && launcher.exists()
        && let Err(e) = fs::remove_file(&launcher)
    {
        notes.push(format!("Could not remove {}: {e}", launcher.display()));
    }
    if let Some(target) = install_path() {
        let dir = target.parent().map(Path::to_path_buf);
        if target.exists()
            && let Err(e) = fs::remove_file(&target)
        {
            notes.push(format!(
                "Could not remove {} ({e}); close deskbell and agents using it, then delete it",
                plain(&target)
            ));
        }
        // On Windows the folder is ours alone; remove it once empty.
        if cfg!(windows)
            && let Some(dir) = dir
        {
            if let Ok(entries) = fs::read_dir(&dir) {
                for entry in entries.flatten() {
                    if entry.file_name().to_string_lossy().starts_with(BINARY) {
                        let _ = fs::remove_file(entry.path());
                    }
                }
            }
            let _ = fs::remove_dir(&dir);
        }
    }
    notes
}

/// Start menu shortcut (Windows) or app-launcher entry (Linux).
fn launcher_path() -> Option<PathBuf> {
    let dirs = directories::BaseDirs::new()?;
    if cfg!(windows) {
        Some(
            dirs.config_dir()
                .join(r"Microsoft\Windows\Start Menu\Programs\Deskbell.lnk"),
        )
    } else if cfg!(target_os = "macos") {
        None
    } else {
        Some(dirs.data_dir().join("applications/deskbell.desktop"))
    }
}

fn add_launcher(target: &Path) -> Result<(), String> {
    let Some(path) = launcher_path() else {
        return Ok(());
    };
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let quote = |p: &Path| plain(p).replace('\'', "''");
        let script = format!(
            "$s = (New-Object -ComObject WScript.Shell).CreateShortcut('{}'); \
             $s.TargetPath = '{}'; $s.WorkingDirectory = '{}'; \
             $s.Description = 'Deskbell'; $s.Save()",
            quote(&path),
            quote(target),
            quote(target.parent().unwrap_or(target)),
        );
        let status = std::process::Command::new("powershell")
            .args(["-NoProfile", "-NonInteractive", "-Command", &script])
            .creation_flags(CREATE_NO_WINDOW)
            .status()
            .map_err(|e| format!("Could not create the Start menu shortcut: {e}"))?;
        if !status.success() {
            return Err("Could not create the Start menu shortcut".into());
        }
        Ok(())
    }
    #[cfg(not(windows))]
    {
        let entry = format!(
            "[Desktop Entry]\nType=Application\nName=Deskbell\nComment=A desk bell AI agents ring to ask for your mouse and keyboard\nExec={}\nTerminal=false\nCategories=Utility;\n",
            autostart::desktop_exec(target)
        );
        fs::write(&path, entry).map_err(|e| format!("Could not write {}: {e}", path.display()))
    }
}
