//! State shared between the UI thread, the cursor monitor and HTTP handlers.

use crate::broker::{Broker, Status};
use crate::config::Config;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Condvar, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

pub struct Session {
    /// `clientInfo.name` from the MCP initialize request.
    pub client_name: String,
    /// Name the agent chose for itself in `request_control` (overrides client_name).
    pub agent_name: Option<String>,
}

impl Session {
    pub fn display_name(&self) -> String {
        self.agent_name
            .clone()
            .unwrap_or_else(|| self.client_name.clone())
    }
}

pub struct Shared {
    broker: Mutex<Broker>,
    changed: Condvar,
    pub sessions: Mutex<HashMap<String, Session>>,
    pub config: Mutex<Config>,
    egui_ctx: OnceLock<eframe::egui::Context>,
    /// Set by the monitor if the cursor position cannot be read (e.g. Wayland).
    pub cursor_error: Mutex<Option<String>>,
    /// The bell window in cursor coordinates (min x, min y, max x, max y),
    /// so the monitor knows when the cursor is near it.
    pub window_rect: Mutex<Option<[i32; 4]>>,
    /// The low-level input hook is installed (Windows).
    pub hook_active: AtomicBool,
}

impl Shared {
    pub fn new(config: Config) -> Self {
        Self {
            broker: Mutex::new(Broker::new(config.broker_settings())),
            changed: Condvar::new(),
            sessions: Mutex::new(HashMap::new()),
            config: Mutex::new(config),
            egui_ctx: OnceLock::new(),
            cursor_error: Mutex::new(None),
            window_rect: Mutex::new(None),
            hook_active: AtomicBool::new(false),
        }
    }

    /// Physical input is being told apart from injected input right now.
    pub fn hardware_detection(&self) -> bool {
        self.hook_active.load(Ordering::Relaxed)
            && self.config.lock().unwrap().detection == crate::config::Detection::Hardware
    }

    pub fn set_egui_ctx(&self, ctx: eframe::egui::Context) {
        let _ = self.egui_ctx.set(ctx);
    }

    fn lock(&self) -> MutexGuard<'_, Broker> {
        self.broker.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Redraw the bell soon.
    pub fn repaint(&self) {
        if let Some(ctx) = self.egui_ctx.get() {
            ctx.request_repaint();
        }
    }

    /// Whether `pos` is over the bell's window or just outside it.
    pub fn cursor_near_window(&self, pos: (i32, i32)) -> bool {
        const MARGIN: i32 = 8;
        self.window_rect
            .lock()
            .unwrap()
            .is_some_and(|[x0, y0, x1, y1]| {
                (x0 - MARGIN..=x1 + MARGIN).contains(&pos.0)
                    && (y0 - MARGIN..=y1 + MARGIN).contains(&pos.1)
            })
    }

    /// Record why the cursor can't be read (`None`: it can again).
    pub fn set_cursor_error(&self, error: Option<String>) {
        let mut current = self.cursor_error.lock().unwrap();
        if *current != error {
            *current = error;
            drop(current);
            self.repaint();
        }
    }

    fn notify(&self) {
        self.changed.notify_all();
        self.repaint();
    }

    /// Run `f` against the broker, then advance timers and wake anyone
    /// waiting for a change.
    pub fn with<R>(&self, f: impl FnOnce(&mut Broker, Instant) -> R) -> R {
        let mut b = self.lock();
        let v0 = b.version();
        let now = Instant::now();
        let r = f(&mut b, now);
        b.tick(now);
        let changed = b.version() != v0;
        drop(b);
        if changed {
            self.notify();
        }
        r
    }

    /// Request the mouse and wait up to `wait` for a decision.
    pub fn request_and_wait(
        &self,
        session: &str,
        agent: &str,
        task: &str,
        wait: Duration,
    ) -> Status {
        let deadline = Instant::now() + wait;
        let mut b = self.lock();
        let mut v0 = b.version();
        let mut st = b.request(session, agent, task, Instant::now());
        loop {
            // Only announce changes made since the last announcement: otherwise
            // two waiting requests keep waking each other up in a busy loop.
            if b.version() != v0 {
                v0 = b.version();
                self.notify();
            }
            let now = Instant::now();
            if !matches!(st, Status::Queued { .. }) || now >= deadline {
                return st;
            }
            let timeout = (deadline - now).min(Duration::from_millis(500));
            b = self
                .changed
                .wait_timeout(b, timeout)
                .unwrap_or_else(|e| e.into_inner())
                .0;
            let now = Instant::now();
            b.tick(now);
            st = b.poll(session, now);
        }
    }
}
