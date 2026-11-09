//! Finds, downloads and applies updates.
//!
//! Checking and downloading happen in the background. Applying is always the
//! user's click (in settings, where software clicks are ignored in hardware
//! mode): the running binary is swapped for the verified download and the bell
//! restarts itself.
//!
//! deskbell ships as a single binary, so updating means replacing it in place.
//! On Windows a running exe can't be overwritten but can be renamed, so the old
//! one is moved aside (and deleted on the next start, once nothing runs it).

use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{RecvTimeoutError, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use eframe::egui;

use crate::manifest::{self, Manifest};

const REPOSITORY: &str = "https://github.com/H3katonkheir/deskbell";
/// Leaves time for the network to come up after signing in.
const FIRST_CHECK_DELAY: Duration = Duration::from_secs(30);
const CHECK_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);
const MAX_MANIFEST_BYTES: u64 = 16 * 1024;
const MAX_BINARY_BYTES: u64 = 128 * 1024 * 1024;
/// Set on the restarted bell, so it waits for the old one to free the port.
pub const RESTART_ENV: &str = "DESKBELL_AFTER_UPDATE";

pub const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Clone, Debug, PartialEq)]
pub enum State {
    Idle,
    /// Updates can't work for this copy (the reason is shown).
    Unavailable(String),
    Checking,
    UpToDate,
    Downloading(String),
    /// A verified update is downloaded and can be applied.
    Ready(String),
    Installing(String),
    Failed(String),
}

pub enum Request {
    Check,
    Install,
    SetAutomatic(bool),
}

pub struct Handle {
    pub state: Arc<Mutex<State>>,
    requests: Sender<Request>,
}

impl Handle {
    pub fn send(&self, request: Request) {
        let _ = self.requests.send(request);
    }
    pub fn state(&self) -> State {
        self.state.lock().unwrap().clone()
    }
}

struct Ready {
    manifest: Manifest,
    path: PathBuf,
}

/// The page describing the running version.
pub fn release_notes_url() -> String {
    format!("{REPOSITORY}/releases/tag/v{CURRENT_VERSION}")
}

/// Where releases come from. Overriding this is safe, and is how updates are
/// tested: every manifest must still carry a valid signature.
fn base_url() -> String {
    std::env::var("DESKBELL_UPDATE_BASE")
        .unwrap_or_else(|_| format!("{REPOSITORY}/releases/latest/download"))
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(600)))
        .user_agent(format!("deskbell/{CURRENT_VERSION}"))
        .build()
        .into()
}

/// The running binary, if this copy can update itself; otherwise why not.
fn this_binary() -> Result<PathBuf, String> {
    let Some(_) = manifest::ASSET else {
        return Err("Updates aren't available for this platform".into());
    };
    let exe = std::env::current_exe()
        .and_then(fs::canonicalize)
        .map_err(|e| format!("Can't locate the running binary: {e}"))?;
    if is_dev_build(&exe) {
        return Err("Updates are off for development builds".into());
    }
    Ok(exe)
}

/// A debug build, or a binary inside a cargo build folder: updating or
/// installing it would only get in the developer's way.
pub fn is_dev_build(exe: &Path) -> bool {
    cfg!(debug_assertions)
        || exe
            .parent()
            .is_some_and(|d| d.join(".fingerprint").exists())
}

/// Where a downloaded update waits next to the binary (same volume, so the
/// final swap is a rename).
fn staged_path(exe: &Path) -> PathBuf {
    let mut name = exe.file_name().unwrap_or_default().to_os_string();
    name.push(".update");
    exe.with_file_name(name)
}

/// The verified manifest of a newer release, if there is one.
pub fn find_update() -> Result<Option<Manifest>, String> {
    let asset = manifest::ASSET.ok_or("Updates aren't available for this platform")?;
    let url = format!("{}/{}", base_url(), manifest::manifest_name(asset));
    let mut response = agent()
        .get(&url)
        .call()
        .map_err(|e| format!("Could not reach the update server: {e}"))?;
    let mut text = String::new();
    response
        .body_mut()
        .as_reader()
        .take(MAX_MANIFEST_BYTES)
        .read_to_string(&mut text)
        .map_err(|e| format!("Could not read the update details: {e}"))?;

    let manifest: Manifest =
        serde_json::from_str(&text).map_err(|_| "The update details are unreadable".to_owned())?;
    if !manifest.verify(manifest::PUBLIC_KEY) {
        return Err("The update's signature is not valid".to_owned());
    }
    if !manifest.is_for(asset) {
        return Err("The update is for another platform".to_owned());
    }
    Ok(manifest::is_newer(&manifest.version, CURRENT_VERSION).then_some(manifest))
}

fn matches_hash(path: &Path, expected: &str) -> bool {
    fs::read(path).is_ok_and(|bytes| manifest::sha256_hex(&bytes) == expected)
}

/// Downloads the binary named by a verified manifest next to `exe` and checks its hash.
fn download(manifest: &Manifest, exe: &Path) -> Result<PathBuf, String> {
    let path = staged_path(exe);
    if matches_hash(&path, &manifest.sha256) {
        return Ok(path);
    }
    let mut partial = path.clone().into_os_string();
    partial.push(".part");
    let partial = PathBuf::from(partial);
    let url = format!("{}/{}", base_url(), manifest.file);
    let mut response = agent()
        .get(&url)
        .call()
        .map_err(|e| format!("Could not download the update: {e}"))?;
    let mut reader = response.body_mut().as_reader().take(MAX_BINARY_BYTES);
    let mut file = File::create(&partial).map_err(|e| {
        format!(
            "Can't write next to {} ({e}); download the new version by hand",
            exe.display()
        )
    })?;
    std::io::copy(&mut reader, &mut file)
        .map_err(|e| format!("The download was interrupted: {e}"))?;
    file.flush().map_err(|e| e.to_string())?;
    drop(file);

    if !matches_hash(&partial, &manifest.sha256) {
        let _ = fs::remove_file(&partial);
        return Err("The download does not match the signed release".to_owned());
    }
    fs::rename(&partial, &path).map_err(|e| e.to_string())?;
    Ok(path)
}

/// Swaps the running binary for the staged update and starts it.
fn apply(ready: &Ready, exe: &Path) -> Result<(), String> {
    // Check once more right before it replaces the binary.
    let bytes = fs::read(&ready.path).map_err(|e| format!("Could not read the update: {e}"))?;
    if manifest::sha256_hex(&bytes) != ready.manifest.sha256 {
        return Err("The downloaded update has changed".to_owned());
    }
    drop(bytes);

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&ready.path, fs::Permissions::from_mode(0o755))
            .map_err(|e| e.to_string())?;
        // Atomic; processes still running the old binary keep their copy.
        fs::rename(&ready.path, exe).map_err(|e| format!("Could not replace the binary: {e}"))?;
    }
    #[cfg(windows)]
    {
        // A running exe can be renamed but not replaced.
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        let mut old = exe.file_name().unwrap_or_default().to_os_string();
        old.push(format!(".old-{stamp}"));
        let old = exe.with_file_name(old);
        fs::rename(exe, &old).map_err(|e| format!("Could not move the old binary aside: {e}"))?;
        if let Err(e) = fs::rename(&ready.path, exe) {
            let _ = fs::rename(&old, exe);
            return Err(format!("Could not put the new binary in place: {e}"));
        }
    }

    std::process::Command::new(exe)
        .env(RESTART_ENV, "1")
        .spawn()
        .map_err(|e| format!("Updated, but could not restart: {e}. Start deskbell again."))?;
    Ok(())
}

/// Removes binaries left over from earlier updates, in the background. Right
/// after an update the old bell may still be exiting, and an agent's
/// `deskbell mcp` process may run an old binary for longer, so this retries for
/// a while and otherwise leaves the rest for the next start.
pub fn clean_up_old_binaries() {
    let Ok(exe) = this_binary() else { return };
    let (Some(dir), Some(name)) = (exe.parent(), exe.file_name()) else {
        return;
    };
    let dir = dir.to_path_buf();
    let prefix = format!("{}.old-", name.to_string_lossy());
    let remove_all = move || {
        let Ok(entries) = fs::read_dir(&dir) else {
            return true;
        };
        let mut all_gone = true;
        for entry in entries.flatten() {
            if entry.file_name().to_string_lossy().starts_with(&prefix) {
                all_gone &= fs::remove_file(entry.path()).is_ok();
            }
        }
        all_gone
    };
    let _ = std::thread::Builder::new()
        .name("cleanup".into())
        .spawn(move || {
            for _ in 0..30 {
                if remove_all() {
                    return;
                }
                std::thread::sleep(Duration::from_secs(2));
            }
        });
}

/// Starts the thread that checks for updates. `ctx` is repainted on changes
/// and closed when the update has restarted the bell.
pub fn start(ctx: egui::Context, automatic: bool) -> Handle {
    let state = Arc::new(Mutex::new(State::Idle));
    let (requests, inbox) = channel();
    let handle = Handle {
        state: state.clone(),
        requests,
    };
    let exe = match this_binary() {
        Ok(exe) => exe,
        Err(reason) => {
            *state.lock().unwrap() = State::Unavailable(reason);
            return handle;
        }
    };
    let spawned = std::thread::Builder::new()
        .name("updates".into())
        .spawn(move || {
            let set_state = |s: State| {
                *state.lock().unwrap() = s;
                ctx.request_repaint();
            };
            let mut automatic = automatic;
            let mut ready: Option<Ready> = None;
            let mut next_check = Instant::now() + FIRST_CHECK_DELAY;

            // Automatic checks stay quiet when they fail: being offline is not news.
            let check = |ready: &mut Option<Ready>, quiet: bool| {
                if !quiet {
                    set_state(State::Checking);
                }
                let found = find_update().and_then(|found| match found {
                    None => Ok(None),
                    Some(manifest) => {
                        set_state(State::Downloading(manifest.version.clone()));
                        let path = download(&manifest, &exe)?;
                        Ok(Some(Ready { manifest, path }))
                    }
                });
                match found {
                    Ok(Some(update)) => {
                        set_state(State::Ready(update.manifest.version.clone()));
                        *ready = Some(update);
                    }
                    Ok(None) => set_state(State::UpToDate),
                    Err(reason) => match (quiet, ready.is_none()) {
                        (true, true) => set_state(State::Idle),
                        (true, false) => {}
                        (false, _) => set_state(State::Failed(reason)),
                    },
                }
            };

            loop {
                let wait = next_check.saturating_duration_since(Instant::now());
                match inbox.recv_timeout(wait) {
                    Ok(Request::Check) => check(&mut ready, false),
                    Ok(Request::SetAutomatic(on)) => automatic = on,
                    Ok(Request::Install) => {
                        let Some(update) = &ready else { continue };
                        let version = update.manifest.version.clone();
                        set_state(State::Installing(version.clone()));
                        match apply(update, &exe) {
                            Ok(()) => {
                                // The new bell waits for this one to let go of the port.
                                ctx.send_viewport_cmd_to(
                                    egui::ViewportId::ROOT,
                                    egui::ViewportCommand::Close,
                                );
                                return;
                            }
                            Err(reason) => set_state(State::Failed(reason)),
                        }
                    }
                    Err(RecvTimeoutError::Timeout) => {
                        next_check = Instant::now() + CHECK_INTERVAL;
                        if automatic {
                            check(&mut ready, true);
                        }
                    }
                    Err(RecvTimeoutError::Disconnected) => return,
                }
            }
        });
    if let Err(e) = spawned {
        *handle.state.lock().unwrap() = State::Unavailable(format!("Could not start: {e}"));
    }
    handle
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn staged_update_sits_next_to_the_binary() {
        let exe = Path::new("/apps/deskbell.exe");
        assert_eq!(staged_path(exe), Path::new("/apps/deskbell.exe.update"));
    }
}
