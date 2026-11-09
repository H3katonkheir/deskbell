//! The mouse-access state machine. Pure logic, no I/O: every method takes `now`
//! so it can be unit tested deterministically.
//!
//! Rules implemented:
//! * Agents `request` the mouse; requests form a FIFO queue.
//! * While nobody holds the mouse and the queue is non-empty, the bell "rings"
//!   for the request at the head of the queue.
//! * The user grants by clicking the bell (`user_allow`) or denies with a
//!   right-click (`user_deny`).
//! * If the bell rings and the cursor is not moved for `idle_grant`, the head
//!   of the queue is granted automatically.
//! * With `auto_fire` on, the head of the queue is granted without asking as
//!   soon as the user hasn't touched the mouse for `AUTO_FIRE_IDLE` (so taking
//!   control back isn't immediately undone by the next request).
//! * A grant is a lease of `lease` duration that the holder can `renew`.
//! * If the user moves the cursor more than `move_threshold` pixels while an
//!   agent holds the lease, the lease is revoked. The next time that agent
//!   requests the mouse it goes to the *front* of the queue.

use std::collections::{HashMap, HashSet, VecDeque};
use std::time::{Duration, Instant};

/// Queued requests that are not refreshed (by `request` / `poll`) for this
/// long are dropped, so a crashed agent cannot block the queue forever.
pub const STALE_AFTER: Duration = Duration::from_secs(180);
/// After a grant, small cursor movements are ignored for this long
/// (the user's hand leaving the mouse after clicking the bell).
pub const GRANT_GRACE: Duration = Duration::from_millis(1200);
/// After an injected agent action, cursor samples are ignored for this long.
pub const ACTION_GRACE: Duration = Duration::from_millis(200);
/// Auto-fire mode: how long the user must leave the mouse alone before the
/// next request is granted without asking.
pub const AUTO_FIRE_IDLE: Duration = Duration::from_secs(2);

#[derive(Clone, Debug)]
pub struct Settings {
    /// `None` disables auto-granting.
    pub idle_grant: Option<Duration>,
    pub lease: Duration,
    pub move_threshold: i32,
    /// Grant requests without asking for approval.
    pub auto_fire: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            idle_grant: Some(Duration::from_secs(30)),
            lease: Duration::from_secs(60),
            move_threshold: 8,
            auto_fire: false,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Request {
    pub session: String,
    pub agent: String,
    pub task: String,
    pub since: Instant,
    pub last_seen: Instant,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GrantReason {
    UserClick,
    Idle,
    AutoFire,
}

#[derive(Clone, Debug)]
pub struct Lease {
    pub session: String,
    pub agent: String,
    pub task: String,
    pub expires_at: Instant,
    pub reason: GrantReason,
}

/// Why a session no longer holds / waits for the mouse.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ended {
    Released,
    Expired,
    RevokedByMovement,
    RevokedByUser,
    Denied,
    Stale,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Status {
    Holding {
        remaining: Duration,
        reason: GrantReason,
    },
    Queued {
        /// 1-based.
        position: usize,
        /// Only for the head of the queue, when auto-grant is enabled.
        auto_grant_in: Option<Duration>,
    },
    Idle {
        last: Option<Ended>,
    },
}

pub struct Broker {
    pub settings: Settings,
    queue: VecDeque<Request>,
    holder: Option<Lease>,
    /// Sessions that lost the mouse to user movement; their next request jumps the queue.
    priority: HashSet<String>,
    ended: HashMap<String, Ended>,
    cursor: Option<(i32, i32)>,
    /// Position against which movement is measured.
    anchor: Option<(i32, i32)>,
    last_user_move: Option<Instant>,
    ring_since: Option<Instant>,
    acting: u32,
    /// Ignore cursor samples until then (after an injected action).
    suppress_until: Option<Instant>,
    /// Ignore user input until then (the hand leaving the mouse after a grant).
    grace_until: Option<Instant>,
    /// Net hardware displacement since the last reset (hardware detection mode).
    user_disp: (i64, i64),
    version: u64,
}

fn moved_more_than(a: (i32, i32), b: (i32, i32), threshold: i32) -> bool {
    let dx = (a.0 - b.0) as i64;
    let dy = (a.1 - b.1) as i64;
    let t = threshold.max(0) as i64;
    dx * dx + dy * dy > t * t
}

impl Broker {
    pub fn new(settings: Settings) -> Self {
        Self {
            settings,
            queue: VecDeque::new(),
            holder: None,
            priority: HashSet::new(),
            ended: HashMap::new(),
            cursor: None,
            anchor: None,
            last_user_move: None,
            ring_since: None,
            acting: 0,
            suppress_until: None,
            grace_until: None,
            user_disp: (0, 0),
            version: 0,
        }
    }

    /// Increments whenever something observable changes.
    pub fn version(&self) -> u64 {
        self.version
    }
    fn touch(&mut self) {
        self.version = self.version.wrapping_add(1);
    }

    pub fn queue(&self) -> &VecDeque<Request> {
        &self.queue
    }
    pub fn holder(&self) -> Option<&Lease> {
        self.holder.as_ref()
    }
    pub fn cursor(&self) -> Option<(i32, i32)> {
        self.cursor
    }
    /// The request the bell is currently ringing for.
    pub fn ringing(&self) -> Option<&Request> {
        if self.holder.is_none() {
            self.queue.front()
        } else {
            None
        }
    }

    pub fn lease_remaining(&self, now: Instant) -> Option<Duration> {
        self.holder
            .as_ref()
            .map(|l| l.expires_at.saturating_duration_since(now))
    }

    /// Time until the ringing request is auto-granted (if the cursor stays still).
    pub fn auto_grant_in(&self, now: Instant) -> Option<Duration> {
        if self.holder.is_some() || self.queue.is_empty() {
            return None;
        }
        if self.settings.auto_fire {
            return Some(match self.last_user_move {
                Some(m) => AUTO_FIRE_IDLE.saturating_sub(now.saturating_duration_since(m)),
                None => Duration::ZERO,
            });
        }
        let idle = self.settings.idle_grant?;
        let ring_since = self.ring_since?;
        let start = match self.last_user_move {
            Some(m) if m > ring_since => m,
            _ => ring_since,
        };
        Some(idle.saturating_sub(now.saturating_duration_since(start)))
    }

    pub fn status(&self, session: &str, now: Instant) -> Status {
        if let Some(l) = &self.holder
            && l.session == session
        {
            return Status::Holding {
                remaining: l.expires_at.saturating_duration_since(now),
                reason: l.reason,
            };
        }
        if let Some(i) = self.queue.iter().position(|r| r.session == session) {
            return Status::Queued {
                position: i + 1,
                auto_grant_in: if i == 0 {
                    self.auto_grant_in(now)
                } else {
                    None
                },
            };
        }
        Status::Idle {
            last: self.ended.get(session).copied(),
        }
    }

    /// Like `status`, but also marks a queued request as still alive.
    pub fn poll(&mut self, session: &str, now: Instant) -> Status {
        if let Some(r) = self.queue.iter_mut().find(|r| r.session == session) {
            r.last_seen = now;
        }
        self.status(session, now)
    }

    /// Ask for the mouse (idempotent: re-requesting keeps your place).
    pub fn request(&mut self, session: &str, agent: &str, task: &str, now: Instant) -> Status {
        if self.holder.as_ref().is_some_and(|l| l.session == session) {
            return self.status(session, now);
        }
        if let Some(r) = self.queue.iter_mut().find(|r| r.session == session) {
            r.last_seen = now;
            if r.agent != agent || r.task != task {
                r.agent = agent.to_string();
                r.task = task.to_string();
                self.touch();
            }
        } else {
            let req = Request {
                session: session.to_string(),
                agent: agent.to_string(),
                task: task.to_string(),
                since: now,
                last_seen: now,
            };
            if self.priority.remove(session) {
                self.queue.push_front(req);
            } else {
                self.queue.push_back(req);
            }
            self.ended.remove(session);
            self.touch();
        }
        self.tick(now);
        self.status(session, now)
    }

    pub fn renew(&mut self, session: &str, now: Instant) -> Result<Duration, Status> {
        match &mut self.holder {
            Some(l) if l.session == session && now < l.expires_at => {
                l.expires_at = now + self.settings.lease;
                self.version = self.version.wrapping_add(1);
                Ok(self.settings.lease)
            }
            _ => {
                self.tick(now);
                Err(self.status(session, now))
            }
        }
    }

    /// Give the mouse back (or leave the queue). Returns false if the session had nothing.
    pub fn release(&mut self, session: &str, now: Instant) -> bool {
        let mut found = false;
        if self.holder.as_ref().is_some_and(|l| l.session == session) {
            self.end_holder(Ended::Released);
            found = true;
        }
        let before = self.queue.len();
        self.queue.retain(|r| r.session != session);
        if self.queue.len() != before {
            self.ended.insert(session.to_string(), Ended::Released);
            found = true;
        }
        if found {
            self.touch();
            self.tick(now);
        }
        found
    }

    /// Session disconnected: drop all of its state.
    pub fn forget(&mut self, session: &str, now: Instant) {
        self.release(session, now);
        self.priority.remove(session);
        self.ended.remove(session);
    }

    /// The user clicked the bell.
    pub fn user_allow(&mut self, now: Instant) -> bool {
        if self.holder.is_none() && !self.queue.is_empty() {
            self.grant_head(GrantReason::UserClick, now);
            true
        } else {
            false
        }
    }

    /// The user right-clicked the bell: refuse the ringing request.
    pub fn user_deny(&mut self, now: Instant) -> bool {
        if self.holder.is_some() {
            return false;
        }
        let Some(r) = self.queue.pop_front() else {
            return false;
        };
        self.ended.insert(r.session, Ended::Denied);
        self.ring_since = None;
        self.touch();
        self.tick(now);
        true
    }

    /// The user explicitly took the mouse back (e.g. from the settings window).
    pub fn user_revoke(&mut self, now: Instant) -> bool {
        if self.holder.is_none() {
            return false;
        }
        self.end_holder(Ended::RevokedByUser);
        self.tick(now);
        true
    }

    /// Remove a specific queued request (user action from the settings window).
    pub fn user_remove(&mut self, session: &str, now: Instant) {
        let before = self.queue.len();
        self.queue.retain(|r| r.session != session);
        if before != self.queue.len() {
            self.ended.insert(session.to_string(), Ended::Denied);
            if self.queue.is_empty() {
                self.ring_since = None;
            }
            self.touch();
            self.tick(now);
        }
    }

    /// Record the cursor position without judging who moved it
    /// (hardware detection mode; see [`Self::on_user_input`]).
    pub fn observe_cursor(&mut self, pos: (i32, i32)) {
        self.cursor = Some(pos);
    }

    /// Movement detection mode: any cursor movement that isn't one of our own
    /// injected actions is attributed to the user.
    pub fn on_cursor(&mut self, pos: (i32, i32), now: Instant) {
        self.cursor = Some(pos);
        let suppressed = self.acting > 0
            || self.suppress_until.is_some_and(|t| now < t)
            || self.grace_until.is_some_and(|t| now < t);
        match self.anchor {
            None => self.anchor = Some(pos),
            Some(a) if moved_more_than(a, pos, self.settings.move_threshold) => {
                self.anchor = Some(pos);
                if !suppressed {
                    self.user_activity(now);
                }
            }
            Some(_) => {}
        }
    }

    /// Hardware detection mode: `delta` is cursor movement known to come from
    /// the user's physical mouse, `pressed` a physical button press or wheel turn.
    pub fn on_user_input(&mut self, delta: (i32, i32), pressed: bool, now: Instant) {
        if self.grace_until.is_some_and(|t| now < t) {
            self.user_disp = (0, 0);
            return;
        }
        self.user_disp.0 += delta.0 as i64;
        self.user_disp.1 += delta.1 as i64;
        let t = self.settings.move_threshold.max(0) as i64;
        let (dx, dy) = self.user_disp;
        if pressed || dx * dx + dy * dy > t * t {
            self.user_disp = (0, 0);
            self.user_activity(now);
        }
    }

    fn user_activity(&mut self, now: Instant) {
        self.last_user_move = Some(now);
        if let Some(l) = &self.holder {
            let session = l.session.clone();
            self.end_holder(Ended::RevokedByMovement);
            self.priority.insert(session);
        }
        self.touch();
    }

    /// Called before the holder performs an injected mouse action.
    pub fn begin_action(&mut self, session: &str, now: Instant) -> Result<(), Status> {
        self.tick(now);
        match &self.holder {
            Some(l) if l.session == session => {
                self.acting += 1;
                Ok(())
            }
            _ => Err(self.status(session, now)),
        }
    }

    /// Called after an injected mouse action with the resulting cursor position.
    pub fn end_action(&mut self, pos: Option<(i32, i32)>, now: Instant) {
        self.acting = self.acting.saturating_sub(1);
        if let Some(p) = pos {
            self.cursor = Some(p);
            self.anchor = Some(p);
        }
        self.suppress_until = Some(now + ACTION_GRACE);
    }

    pub fn update_settings(&mut self, settings: Settings, now: Instant) {
        self.settings = settings;
        self.reset_motion();
        self.touch();
        self.tick(now);
    }

    /// Advance timers: lease expiry, stale requests, ringing and auto-grant.
    pub fn tick(&mut self, now: Instant) {
        if self.holder.as_ref().is_some_and(|l| now >= l.expires_at) {
            self.end_holder(Ended::Expired);
        }

        let before = self.queue.len();
        let mut stale = Vec::new();
        self.queue.retain(|r| {
            let alive = now.saturating_duration_since(r.last_seen) < STALE_AFTER;
            if !alive {
                stale.push(r.session.clone());
            }
            alive
        });
        if self.queue.len() != before {
            for s in stale {
                self.ended.insert(s, Ended::Stale);
            }
            self.touch();
        }

        if self.holder.is_none() && !self.queue.is_empty() {
            if self.ring_since.is_none() {
                self.ring_since = Some(now);
                self.touch();
            }
            if self.auto_grant_in(now).is_some_and(|d| d.is_zero()) {
                let reason = if self.settings.auto_fire {
                    GrantReason::AutoFire
                } else {
                    GrantReason::Idle
                };
                self.grant_head(reason, now);
            }
        } else if self.holder.is_none() && self.ring_since.is_some() {
            self.ring_since = None;
            self.touch();
        }
    }

    fn grant_head(&mut self, reason: GrantReason, now: Instant) {
        let Some(r) = self.queue.pop_front() else {
            return;
        };
        self.ended.remove(&r.session);
        self.priority.remove(&r.session);
        self.holder = Some(Lease {
            session: r.session,
            agent: r.agent,
            task: r.task,
            expires_at: now + self.settings.lease,
            reason,
        });
        self.ring_since = None;
        self.reset_motion();
        self.grace_until = Some(now + GRANT_GRACE);
        self.touch();
    }

    fn end_holder(&mut self, why: Ended) {
        if let Some(l) = self.holder.take() {
            self.ended.insert(l.session, why);
            self.reset_motion();
            self.touch();
        }
    }

    /// Forget accumulated movement (e.g. when switching detection modes).
    pub fn reset_motion(&mut self) {
        self.anchor = self.cursor;
        self.user_disp = (0, 0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn broker() -> (Broker, Instant) {
        let mut b = Broker::new(Settings::default());
        let t0 = Instant::now();
        b.on_cursor((100, 100), t0);
        (b, t0)
    }
    fn s(secs: u64) -> Duration {
        Duration::from_secs(secs)
    }

    #[test]
    fn click_grants_head_and_queue_follows() {
        let (mut b, t) = broker();
        assert!(matches!(
            b.request("a", "A", "task a", t),
            Status::Queued { position: 1, .. }
        ));
        assert!(matches!(
            b.request("b", "B", "task b", t),
            Status::Queued { position: 2, .. }
        ));
        assert_eq!(b.ringing().unwrap().session, "a");
        assert!(b.user_allow(t + s(1)));
        assert!(matches!(
            b.status("a", t + s(1)),
            Status::Holding {
                reason: GrantReason::UserClick,
                ..
            }
        ));
        assert!(matches!(
            b.status("b", t + s(1)),
            Status::Queued { position: 1, .. }
        ));
        assert!(b.ringing().is_none());
        assert!(b.release("a", t + s(2)));
        assert_eq!(
            b.status("a", t + s(2)),
            Status::Idle {
                last: Some(Ended::Released)
            }
        );
        assert_eq!(b.ringing().unwrap().session, "b");
    }

    #[test]
    fn idle_auto_grant_after_timeout() {
        let (mut b, t) = broker();
        b.request("a", "A", "x", t);
        b.tick(t + s(29));
        assert!(matches!(b.status("a", t + s(29)), Status::Queued { .. }));
        b.tick(t + s(30));
        assert!(matches!(
            b.status("a", t + s(30)),
            Status::Holding {
                reason: GrantReason::Idle,
                ..
            }
        ));
    }

    #[test]
    fn auto_fire_grants_without_asking() {
        let (mut b, t) = broker();
        b.settings.auto_fire = true;
        assert!(matches!(
            b.request("a", "A", "x", t),
            Status::Holding {
                reason: GrantReason::AutoFire,
                ..
            }
        ));
        // Queued behind the holder until it releases, then granted right away.
        assert!(matches!(b.request("b", "B", "y", t), Status::Queued { .. }));
        b.release("a", t + s(1));
        assert!(matches!(
            b.status("b", t + s(1)),
            Status::Holding {
                reason: GrantReason::AutoFire,
                ..
            }
        ));
    }

    #[test]
    fn auto_fire_waits_for_user_to_stop_moving() {
        let (mut b, t) = broker();
        b.settings.auto_fire = true;
        b.request("a", "A", "x", t);
        // The user takes the mouse back...
        b.on_cursor((300, 300), t + s(5));
        assert!(matches!(
            b.status("a", t + s(5)),
            Status::Idle {
                last: Some(Ended::RevokedByMovement)
            }
        ));
        // ...and the agent immediately asks again: it must wait until the user stops.
        assert!(matches!(
            b.request("a", "A", "x", t + s(5)),
            Status::Queued { .. }
        ));
        b.tick(t + s(6));
        assert!(matches!(b.status("a", t + s(6)), Status::Queued { .. }));
        b.tick(t + s(7));
        assert!(matches!(b.status("a", t + s(7)), Status::Holding { .. }));
    }

    #[test]
    fn movement_resets_idle_timer() {
        let (mut b, t) = broker();
        b.request("a", "A", "x", t);
        b.on_cursor((200, 200), t + s(20));
        b.tick(t + s(31));
        assert!(matches!(b.status("a", t + s(31)), Status::Queued { .. }));
        b.tick(t + s(50));
        assert!(matches!(b.status("a", t + s(50)), Status::Holding { .. }));
    }

    #[test]
    fn small_jitter_is_ignored() {
        let (mut b, t) = broker();
        b.request("a", "A", "x", t);
        for i in 0..10 {
            b.on_cursor(
                (100 + i % 3, 100),
                t + Duration::from_millis(100 * i as u64),
            );
        }
        b.tick(t + s(30));
        assert!(matches!(b.status("a", t + s(30)), Status::Holding { .. }));
    }

    #[test]
    fn lease_expires_and_can_be_renewed() {
        let (mut b, t) = broker();
        b.request("a", "A", "x", t);
        b.user_allow(t);
        assert_eq!(b.renew("a", t + s(50)), Ok(s(60)));
        b.tick(t + s(100));
        assert!(matches!(b.status("a", t + s(100)), Status::Holding { .. }));
        b.tick(t + s(110));
        assert_eq!(
            b.status("a", t + s(110)),
            Status::Idle {
                last: Some(Ended::Expired)
            }
        );
        assert!(b.renew("a", t + s(111)).is_err());
    }

    #[test]
    fn user_movement_revokes_and_gives_priority() {
        let (mut b, t) = broker();
        b.request("a", "A", "x", t);
        b.request("b", "B", "y", t);
        b.user_allow(t);
        // within the grace period: ignored
        b.on_cursor((150, 150), t + Duration::from_millis(500));
        assert!(matches!(b.status("a", t + s(1)), Status::Holding { .. }));
        // later: revoked
        b.on_cursor((300, 300), t + s(5));
        assert_eq!(
            b.status("a", t + s(5)),
            Status::Idle {
                last: Some(Ended::RevokedByMovement)
            }
        );
        assert_eq!(b.ringing().unwrap().session, "b");
        // re-request jumps ahead of b
        assert!(matches!(
            b.request("a", "A", "x", t + s(6)),
            Status::Queued { position: 1, .. }
        ));
        assert!(matches!(
            b.status("b", t + s(6)),
            Status::Queued { position: 2, .. }
        ));
    }

    #[test]
    fn agent_actions_do_not_revoke() {
        let (mut b, t) = broker();
        b.request("a", "A", "x", t);
        b.user_allow(t);
        let t1 = t + s(5);
        b.begin_action("a", t1).unwrap();
        b.on_cursor((800, 600), t1);
        b.end_action(Some((800, 600)), t1);
        b.on_cursor((800, 600), t1 + Duration::from_millis(50));
        assert!(matches!(b.status("a", t1), Status::Holding { .. }));
        b.on_cursor((820, 640), t1 + s(1));
        assert_eq!(
            b.status("a", t1 + s(1)),
            Status::Idle {
                last: Some(Ended::RevokedByMovement)
            }
        );
    }

    #[test]
    fn actions_require_lease() {
        let (mut b, t) = broker();
        assert!(b.begin_action("a", t).is_err());
        b.request("a", "A", "x", t);
        assert!(b.begin_action("a", t).is_err());
    }

    #[test]
    fn deny_and_stale() {
        let (mut b, t) = broker();
        b.request("a", "A", "x", t);
        b.request("b", "B", "y", t);
        assert!(b.user_deny(t + s(1)));
        assert_eq!(
            b.status("a", t + s(1)),
            Status::Idle {
                last: Some(Ended::Denied)
            }
        );
        // b never polls again -> dropped
        b.settings.idle_grant = None;
        b.tick(t + STALE_AFTER + s(1));
        assert_eq!(
            b.status("b", t + STALE_AFTER + s(1)),
            Status::Idle {
                last: Some(Ended::Stale)
            }
        );
    }

    #[test]
    fn hardware_input_revokes_but_injected_moves_do_not() {
        let (mut b, t) = broker();
        b.request("a", "A", "x", t);
        b.request("b", "B", "y", t);
        b.user_allow(t);
        // the agent moves the cursor with its own tools: only observed
        b.observe_cursor((900, 700));
        b.on_user_input((0, 0), false, t + s(3));
        assert!(matches!(b.status("a", t + s(3)), Status::Holding { .. }));
        // physical jitter back and forth stays below the threshold
        for i in 0..20 {
            let d = if i % 2 == 0 { 3 } else { -3 };
            b.on_user_input((d, 0), false, t + s(4));
        }
        assert!(matches!(b.status("a", t + s(4)), Status::Holding { .. }));
        // a real movement revokes
        b.on_user_input((5, 0), false, t + s(5));
        b.on_user_input((5, 2), false, t + s(5));
        assert_eq!(
            b.status("a", t + s(5)),
            Status::Idle {
                last: Some(Ended::RevokedByMovement)
            }
        );
        assert!(matches!(
            b.request("a", "A", "x", t + s(6)),
            Status::Queued { position: 1, .. }
        ));
    }

    #[test]
    fn hardware_click_revokes_and_grace_period_applies() {
        let (mut b, t) = broker();
        b.request("a", "A", "x", t);
        b.user_allow(t);
        b.on_user_input((50, 50), true, t + Duration::from_millis(300));
        assert!(matches!(b.status("a", t + s(1)), Status::Holding { .. }));
        b.on_user_input((0, 0), true, t + s(2));
        assert_eq!(
            b.status("a", t + s(2)),
            Status::Idle {
                last: Some(Ended::RevokedByMovement)
            }
        );
    }

    #[test]
    fn hardware_input_resets_idle_timer() {
        let (mut b, t) = broker();
        b.request("a", "A", "x", t);
        b.on_user_input((0, 20), false, t + s(25));
        b.tick(t + s(31));
        assert!(matches!(b.status("a", t + s(31)), Status::Queued { .. }));
        b.tick(t + s(55));
        assert!(matches!(b.status("a", t + s(55)), Status::Holding { .. }));
    }

    #[test]
    fn auto_grant_disabled() {
        let (mut b, t) = broker();
        b.settings.idle_grant = None;
        b.request("a", "A", "x", t);
        b.tick(t + s(100));
        assert!(matches!(
            b.status("a", t + s(100)),
            Status::Queued {
                auto_grant_in: None,
                ..
            }
        ));
    }
}
