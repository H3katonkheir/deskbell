//! "Start at login": a per-user registration with the OS, read back from the
//! OS itself (not the config file) so the switch always shows the truth.
//!
//! * Windows: a value under `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`.
//! * macOS: a LaunchAgent in `~/Library/LaunchAgents`.
//! * Linux: an XDG autostart entry in `~/.config/autostart`.

use std::path::Path;

/// Path without Windows' `\\?\` prefix (from `canonicalize`), for places a
/// person or another program reads.
pub fn plain(path: &Path) -> String {
    let s = path.display().to_string();
    match s.strip_prefix(r"\\?\") {
        Some(rest) if !rest.starts_with("UNC") => rest.to_string(),
        _ => s,
    }
}

#[cfg(windows)]
mod imp {
    use super::{Path, plain};
    use windows_sys::Win32::System::Registry::{
        HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_SZ, RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW,
    };

    const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
    const VALUE: &str = "deskbell";

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(Some(0)).collect()
    }

    pub fn is_enabled() -> bool {
        let (key, value) = (wide(RUN_KEY), wide(VALUE));
        let mut size = 0u32;
        let r = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                key.as_ptr(),
                value.as_ptr(),
                RRF_RT_REG_SZ,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut size,
            )
        };
        r == 0
    }

    pub fn enable(exe: &Path) -> Result<(), String> {
        let (key, value) = (wide(RUN_KEY), wide(VALUE));
        let data = wide(&format!("\"{}\"", plain(exe)));
        let r = unsafe {
            RegSetKeyValueW(
                HKEY_CURRENT_USER,
                key.as_ptr(),
                value.as_ptr(),
                REG_SZ,
                data.as_ptr().cast(),
                (data.len() * 2) as u32,
            )
        };
        if r == 0 {
            Ok(())
        } else {
            Err(format!("Could not add the login entry (error {r})"))
        }
    }

    pub fn disable() -> Result<(), String> {
        let (key, value) = (wide(RUN_KEY), wide(VALUE));
        let r = unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, key.as_ptr(), value.as_ptr()) };
        // 2 = ERROR_FILE_NOT_FOUND: already off.
        if r == 0 || r == 2 {
            Ok(())
        } else {
            Err(format!("Could not remove the login entry (error {r})"))
        }
    }
}

#[cfg(not(windows))]
mod imp {
    use std::path::{Path, PathBuf};

    #[cfg(target_os = "macos")]
    fn home() -> Option<PathBuf> {
        directories::BaseDirs::new().map(|d| d.home_dir().to_path_buf())
    }

    #[cfg(target_os = "macos")]
    fn entry() -> Option<PathBuf> {
        home().map(|h| h.join("Library/LaunchAgents/dev.deskbell.deskbell.plist"))
    }
    #[cfg(not(target_os = "macos"))]
    fn entry() -> Option<PathBuf> {
        directories::BaseDirs::new().map(|d| d.config_dir().join("autostart/deskbell.desktop"))
    }

    #[cfg(target_os = "macos")]
    fn contents(exe: &Path) -> String {
        let exe = exe
            .display()
            .to_string()
            .replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;");
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key><string>dev.deskbell.deskbell</string>
    <key>ProgramArguments</key><array><string>{exe}</string></array>
    <key>RunAtLoad</key><true/>
</dict>
</plist>
"#
        )
    }
    #[cfg(not(target_os = "macos"))]
    fn contents(exe: &Path) -> String {
        format!(
            "[Desktop Entry]\nType=Application\nName=Deskbell\nComment=A desk bell AI agents ring to ask for your mouse and keyboard\nExec={}\nX-GNOME-Autostart-enabled=true\n",
            super::desktop_exec(exe)
        )
    }

    pub fn is_enabled() -> bool {
        entry().is_some_and(|p| p.exists())
    }

    pub fn enable(exe: &Path) -> Result<(), String> {
        let path = entry().ok_or("Can't find the home folder")?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        std::fs::write(&path, contents(exe))
            .map_err(|e| format!("Could not write {}: {e}", path.display()))
    }

    pub fn disable() -> Result<(), String> {
        match entry() {
            Some(p) if p.exists() => std::fs::remove_file(&p)
                .map_err(|e| format!("Could not remove {}: {e}", p.display())),
            _ => Ok(()),
        }
    }
}

pub use imp::{disable, enable, is_enabled};

/// `exe` quoted for a `.desktop` file's `Exec=` key.
#[cfg_attr(any(windows, target_os = "macos"), allow(dead_code))]
pub fn desktop_exec(exe: &Path) -> String {
    let mut out = String::from('"');
    for c in exe.display().to_string().chars() {
        if matches!(c, '"' | '`' | '$' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('"');
    out
}

/// Keeps an enabled login entry pointing at `exe` (e.g. after installing).
pub fn retarget(exe: &Path) -> Result<(), String> {
    if is_enabled() { enable(exe) } else { Ok(()) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_strips_the_verbatim_prefix() {
        assert_eq!(plain(Path::new(r"\\?\C:\a\b.exe")), r"C:\a\b.exe");
        assert_eq!(plain(Path::new(r"C:\a\b.exe")), r"C:\a\b.exe");
        assert_eq!(
            plain(Path::new(r"\\?\UNC\server\share")),
            r"\\?\UNC\server\share"
        );
    }

    #[test]
    fn desktop_exec_quotes_and_escapes() {
        assert_eq!(
            desktop_exec(Path::new("/home/a b/deskbell")),
            "\"/home/a b/deskbell\""
        );
        assert_eq!(desktop_exec(Path::new("/x/$y")), "\"/x/\\$y\"");
    }
}
