//! The bell widget: a small transparent, undecorated, always-on-top window.
//!
//! Transparent areas of the window are made click-through by toggling
//! "mouse passthrough" depending on whether the (globally sampled) cursor is
//! over an interactive part (the bell or its hover menu).

use crate::broker::GrantReason;
use crate::config::{Config, Theme};
use crate::state::Shared;
use eframe::egui::{
    self, Align2, Color32, FontId, Id, Painter, Pos2, Rect, Sense, Shape, Stroke, StrokeKind, Vec2,
    ViewportCommand, pos2, text::LayoutJob, text::TextFormat, vec2,
};
use std::sync::Arc;
use std::time::{Duration, Instant};

const WIN: Vec2 = vec2(316.0, 214.0);
/// Bell center inside the window (room on the right and below for the hover menu).
const BELL_C: Pos2 = pos2(WIN.x - 56.0, 162.0);
const SETTINGS_TITLE: &str = "Deskbell - Settings";
const DISC_R: f32 = 27.0;
const BELL_S: f32 = 31.0;
const ICON_R: f32 = 11.0;
/// Distance of the hover-menu icons from the bell center.
const MENU_R: f32 = DISC_R + ICON_R + 6.0;
/// Hover-menu items, top to bottom along the arc.
const MENU_ITEMS: usize = 4;
const CLOSE: usize = 0;
const SETTINGS: usize = 1;
const MOVE: usize = 2;
const AUTO_FIRE: usize = 3;
/// Longest the idle bell goes without redrawing; a safety net, since every
/// change that matters asks for a repaint itself.
const IDLE_REPAINT: Duration = Duration::from_secs(2);
/// The window position is saved once it has stayed put this long.
const POS_SAVE_DELAY: Duration = Duration::from_secs(1);

pub fn run(shared: Arc<Shared>, open_settings: bool) -> eframe::Result {
    let pos = shared.config.lock().unwrap().window_pos;
    let mut viewport = egui::ViewportBuilder::default()
        .with_title("Deskbell")
        .with_app_id("deskbell")
        .with_inner_size(WIN)
        .with_resizable(false)
        .with_decorations(false)
        .with_transparent(true)
        .with_has_shadow(false)
        .with_always_on_top()
        .with_taskbar(false);
    if let Some(p) = pos {
        viewport = viewport.with_position(p);
    }
    #[allow(unused_mut)]
    let mut options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    // Global cursor tracking and window placement don't work on native
    // Wayland; prefer X11 (XWayland) when it's available.
    #[cfg(target_os = "linux")]
    if std::env::var_os("DISPLAY").is_some() {
        options.event_loop_builder = Some(Box::new(|b| {
            use winit::platform::x11::EventLoopBuilderExtX11;
            b.with_x11();
        }));
    }

    eframe::run_native(
        "deskbell",
        options,
        Box::new(move |cc| {
            shared.set_egui_ctx(cc.egui_ctx.clone());
            setup_style(&cc.egui_ctx, shared.config.lock().unwrap().theme);
            #[cfg(windows)]
            crate::winframe::apply(cc);
            // Started after the window exists so DPI awareness is already set (Windows).
            crate::input::spawn_monitor(shared.clone());
            let auto_update = shared.config.lock().unwrap().auto_update;
            let updates = crate::update::start(cc.egui_ctx.clone(), auto_update);
            let mut app = BellApp::new(shared, pos.is_some(), updates);
            app.show_settings = open_settings;
            Ok(Box::new(app))
        }),
    )
}

struct BellApp {
    shared: Arc<Shared>,
    positioned: bool,
    passthrough: Option<bool>,
    menu_until: Option<Instant>,
    show_settings: bool,
    draft: Config,
    saved_msg: Option<(String, Instant)>,
    last_pos: Option<Pos2>,
    pos_dirty_since: Option<Instant>,
    /// When a software-generated click on the widget was last ignored.
    rejected_at: Option<Instant>,
    /// Last time the bell was hovered or had something to show.
    last_active: Instant,
    settings_icon: Arc<egui::IconData>,
    /// Windows: the settings window's taskbar icon has been set.
    settings_icon_set: bool,
    /// Selected tab in "Connect your agents".
    client_tab: usize,
    updates: crate::update::Handle,
    /// "Start at login" as last read from the OS (refreshed when settings open).
    autostart_on: Option<bool>,
    /// Looked up when settings open (see [`SettingsInfo`]).
    settings_info: Option<SettingsInfo>,
    /// The port the server listens on (changing it needs a restart).
    port: u16,
}

impl BellApp {
    fn new(shared: Arc<Shared>, positioned: bool, updates: crate::update::Handle) -> Self {
        let draft = shared.config.lock().unwrap().clone();
        let port = draft.port;
        Self {
            shared,
            positioned,
            passthrough: None,
            menu_until: None,
            show_settings: false,
            draft,
            saved_msg: None,
            last_pos: None,
            pos_dirty_since: None,
            rejected_at: None,
            last_active: Instant::now(),
            settings_icon: Arc::new(crate::icon::icon_data(64)),
            settings_icon_set: false,
            client_tab: 0,
            updates,
            autostart_on: None,
            settings_info: None,
            port,
        }
    }

    fn save_config(&self, f: impl FnOnce(&mut Config)) {
        let mut cfg = self.shared.config.lock().unwrap();
        f(&mut cfg);
        let _ = cfg.save();
    }

    fn set_auto_fire(&mut self, on: bool) {
        self.draft.auto_fire = on;
        let mut cfg = self.shared.config.lock().unwrap();
        cfg.auto_fire = on;
        let _ = cfg.save();
        let settings = cfg.broker_settings();
        drop(cfg);
        self.shared.with(|b, now| b.update_settings(settings, now));
    }
}

fn bell_rect() -> Rect {
    Rect::from_center_size(BELL_C, Vec2::splat(DISC_R * 2.0 + 6.0))
}
/// Direction of menu icon `i` from the bell: an arc down the bell's right
/// side, symmetric about its horizontal centre line, close at the top.
fn icon_dir(i: usize) -> Vec2 {
    let spread = 35.0 * (MENU_ITEMS - 1) as f32;
    let a = (-spread / 2.0 + 35.0 * i as f32).to_radians();
    vec2(a.cos(), a.sin())
}
fn icon_center(i: usize) -> Pos2 {
    BELL_C + icon_dir(i) * MENU_R
}
fn icon_rect(i: usize) -> Rect {
    Rect::from_center_size(icon_center(i), Vec2::splat(ICON_R * 2.0 + 2.0))
}
/// Area that keeps the menu open: the icons and the gap between them and the bell.
fn menu_rect() -> Rect {
    let icons = (0..MENU_ITEMS).fold(Rect::NOTHING, |r, i| r.union(icon_rect(i)));
    icons
        .expand(4.0)
        .union(Rect::from_min_max(pos2(BELL_C.x, icons.min.y), icons.max))
}

struct View {
    agent: Option<String>,
    task: String,
    holding: bool,
    reason: Option<GrantReason>,
    remaining: Option<Duration>,
    lease: Duration,
    auto_in: Option<Duration>,
    waiting: usize,
    cursor: Option<(i32, i32)>,
    /// The user's typing is detected and takes control back.
    typing_counts: bool,
    auto_fire: bool,
}

impl eframe::App for BellApp {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0, 0.0, 0.0, 0.0]
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let now = Instant::now();
        let typing_counts = self.shared.hardware_detection()
            && self.shared.config.lock().unwrap().keyboard_takes_back;
        let view = self.shared.with(|b, now| {
            let (agent, task, holding, reason) = match (b.holder(), b.ringing()) {
                (Some(l), _) => (Some(l.agent.clone()), l.task.clone(), true, Some(l.reason)),
                (None, Some(r)) => (Some(r.agent.clone()), r.task.clone(), false, None),
                _ => (None, String::new(), false, None),
            };
            View {
                agent,
                task,
                holding,
                reason,
                remaining: b.lease_remaining(now),
                lease: b.settings.lease,
                auto_in: b.auto_grant_in(now),
                waiting: b.queue().len(),
                cursor: b.cursor(),
                typing_counts,
                auto_fire: b.settings.auto_fire,
            }
        });
        let ringing = view.agent.is_some() && !view.holding;

        // ---- placement & persistence -------------------------------------------------
        let vp = ctx.input(|i| i.viewport().clone());
        if !self.positioned
            && let Some(m) = vp.monitor_size
        {
            ctx.send_viewport_cmd(ViewportCommand::OuterPosition(pos2(
                (m.x - WIN.x - 16.0).max(0.0),
                (m.y - WIN.y - 56.0).max(0.0),
            )));
            self.positioned = true;
        }
        if let Some(outer) = vp.outer_rect {
            if self.last_pos.is_some_and(|p| p != outer.min) {
                self.pos_dirty_since = Some(now);
            }
            self.last_pos = Some(outer.min);
        }
        if self
            .pos_dirty_since
            .is_some_and(|t| now - t >= POS_SAVE_DELAY)
        {
            self.pos_dirty_since = None;
            if let Some(p) = self.last_pos {
                self.save_config(|c| c.window_pos = Some([p.x, p.y]));
            }
        }

        // ---- where is the cursor relative to the window? ----------------------------
        // Points per unit of cursor coordinates: physical pixels, except on
        // macOS where the cursor is reported in points.
        let ppp = if cfg!(target_os = "macos") {
            1.0
        } else {
            vp.native_pixels_per_point.unwrap_or(1.0)
        };
        if let Some(inner) = vp.inner_rect {
            let r = [
                (inner.min.x * ppp).floor() as i32,
                (inner.min.y * ppp).floor() as i32,
                (inner.max.x * ppp).ceil() as i32,
                (inner.max.y * ppp).ceil() as i32,
            ];
            *self.shared.window_rect.lock().unwrap() = Some(r);
        }
        let local_cursor: Option<Pos2> = match (view.cursor, vp.inner_rect) {
            (Some((x, y)), Some(inner)) => {
                Some(pos2(x as f32 / ppp, y as f32 / ppp) - inner.min.to_vec2())
            }
            _ => ctx.input(|i| i.pointer.hover_pos()),
        };
        let over_bell = local_cursor.is_some_and(|p| bell_rect().contains(p));
        let menu_open = self.menu_until.is_some_and(|t| now < t);
        let over_menu = menu_open && local_cursor.is_some_and(|p| menu_rect().contains(p));
        if over_bell || over_menu {
            self.menu_until = Some(now + Duration::from_millis(600));
        }
        let menu_open = self.menu_until.is_some_and(|t| now < t);

        // Click-through everywhere except the bell and the open menu. If we can't
        // locate the cursor (e.g. Wayland) stay clickable.
        let want_passthrough =
            vp.inner_rect.is_some() && view.cursor.is_some() && !over_bell && !over_menu;
        if self.passthrough != Some(want_passthrough) {
            ctx.send_viewport_cmd(ViewportCommand::MousePassthrough(want_passthrough));
            self.passthrough = Some(want_passthrough);
        }

        // ---- interaction ------------------------------------------------------------
        // In hardware mode, clicks generated by software (e.g. an agent trying
        // to approve itself or close the bell) are ignored.
        let hardware = self.shared.hardware_detection();
        let synthetic = || hardware && crate::hook::last_button_was_injected();
        // Not keyboard-focusable: an agent must not be able to Tab to the bell
        // (or a menu icon) and "click" it with an injected key press.
        let bell = ui.interact(bell_rect(), Id::unique("bell"), Sense::CLICK);
        if bell.clicked() || bell.secondary_clicked() {
            if synthetic() {
                self.rejected_at = Some(now);
            } else if bell.clicked() {
                self.shared.with(|b, now| b.user_allow(now));
            } else {
                self.shared.with(|b, now| b.user_deny(now));
            }
        }
        let mut icon_hover = [false; MENU_ITEMS];
        if menu_open {
            for (i, hov) in icon_hover.iter_mut().enumerate() {
                let resp = ui.interact(
                    icon_rect(i),
                    Id::unique(("menu", i)),
                    Sense::CLICK | Sense::DRAG,
                );
                *hov = resp.hovered();
                let pressed = resp.hovered() && ctx.input(|i| i.pointer.primary_pressed());
                // Software clicks are ignored, so agents can't e.g. turn on
                // auto-fire (approve themselves) or close the bell.
                if (resp.clicked() || pressed) && synthetic() {
                    self.rejected_at = Some(now);
                    continue;
                }
                match i {
                    CLOSE if resp.clicked() => {
                        if let Some(p) = self.last_pos {
                            self.save_config(|c| c.window_pos = Some([p.x, p.y]));
                        }
                        ctx.send_viewport_cmd(ViewportCommand::Close);
                    }
                    SETTINGS if resp.clicked() => {
                        self.draft = self.shared.config.lock().unwrap().clone();
                        self.show_settings = true;
                    }
                    MOVE if pressed => {
                        ctx.send_viewport_cmd(ViewportCommand::StartDrag);
                    }
                    AUTO_FIRE if resp.clicked() => self.set_auto_fire(!view.auto_fire),
                    _ => {}
                }
            }
        }
        let menu_anim = ctx.animate_bool_with_time(Id::unique("menu-anim"), menu_open, 0.25);

        // ---- painting ---------------------------------------------------------------
        let badge = if view.holding {
            view.waiting
        } else {
            view.waiting.saturating_sub(1)
        };
        let cursor_error = self.shared.cursor_error.lock().unwrap().is_some();
        let rejected = self
            .rejected_at
            .is_some_and(|t| now - t < Duration::from_secs(4));

        // After a quiet spell the bell fades to a plain outline. `full` is 1.0
        // for the normal look and 0.0 for the outline.
        let busy = ringing
            || view.holding
            || badge > 0
            || cursor_error
            || rejected
            || menu_open
            || over_bell
            || self.show_settings;
        if busy {
            self.last_active = now;
        }
        let (simplify, after) = {
            let c = self.shared.config.lock().unwrap();
            (c.simplify_when_idle, c.simplify_after_secs.max(1))
        };
        let quiet = simplify && now - self.last_active >= Duration::from_secs(after);
        let full = ctx.animate_bool_with_time(Id::unique("bell-full"), !quiet, 0.6);

        let p = ui.painter().clone();
        let t = ctx.input(|i| i.time) as f32;
        paint_disc(&p, &view, ringing, t, bell.hovered(), full);
        let swing = if ringing {
            let ph = t % 1.6;
            if ph < 0.9 {
                0.30 * (ph * 22.0).sin() * (1.0 - ph / 0.9)
            } else {
                0.0
            }
        } else {
            0.0
        };
        let (body, shade) = match (ringing, view.holding) {
            (true, _) => (
                Color32::from_rgb(255, 196, 40),
                Color32::from_rgb(214, 148, 18),
            ),
            (_, true) => (
                Color32::from_rgb(228, 190, 82),
                Color32::from_rgb(178, 140, 50),
            ),
            _ => (
                Color32::from_rgb(196, 166, 84),
                Color32::from_rgb(150, 122, 54),
            ),
        };
        if full > 0.0 {
            paint_bell(
                &p,
                BELL_C + vec2(0.0, 2.0),
                BELL_S,
                swing,
                body,
                shade,
                full,
            );
        }
        if full < 1.0 {
            paint_bell_outline(&p, BELL_C + vec2(0.0, 2.0), BELL_S, 1.0 - full);
        }

        if badge > 0 {
            // Sits where the menu fans out, so it steps aside while the menu shows.
            let a = 1.0 - menu_anim;
            let c = BELL_C + vec2(19.0, -19.0);
            p.circle(
                c,
                9.0,
                Color32::from_rgb(220, 50, 50).gamma_multiply(a),
                Stroke::new(1.5, Color32::WHITE.gamma_multiply(a)),
            );
            p.text(
                c,
                Align2::CENTER_CENTER,
                format!("+{badge}"),
                FontId::proportional(10.0),
                Color32::WHITE.gamma_multiply(a),
            );
        }
        if cursor_error {
            let c = BELL_C + vec2(-19.0, -19.0);
            p.circle_filled(c, 8.0, Color32::from_rgb(230, 120, 20));
            p.text(
                c,
                Align2::CENTER_CENTER,
                "!",
                FontId::proportional(12.0),
                Color32::WHITE,
            );
        }

        if view.auto_fire {
            let c = BELL_C + vec2(-19.0, 19.0);
            if full > 0.0 {
                paint_auto_fire_badge(&p, c, full);
            }
            if full < 1.0 {
                // Keep auto-fire visible in the outline look, as a bare flame.
                let a = 1.0 - full;
                paint_flame(&p, c, 16.5, outline_shadow(a), outline_shadow(a));
                paint_flame(&p, c, 14.0, outline_color(a), outline_shadow(a));
            }
        }
        if rejected {
            let r = Rect::from_center_size(
                pos2(BELL_C.x - DISC_R - 84.0, BELL_C.y + 26.0),
                vec2(150.0, 16.0),
            );
            p.rect_filled(r, 4.0, Color32::from_rgb(190, 40, 40));
            p.text(
                r.center(),
                Align2::CENTER_CENTER,
                "Ignored a synthetic click",
                FontId::proportional(11.0),
                Color32::WHITE,
            );
        }
        if self.shared.config.lock().unwrap().show_bubble && view.agent.is_some() {
            paint_bubble(&p, &view, ui.visuals().dark_mode);
        }
        if menu_anim > 0.0 {
            let update_ready = matches!(self.updates.state(), crate::update::State::Ready(_));
            paint_menu(&p, icon_hover, menu_anim, view.auto_fire, update_ready);
        }

        if self.show_settings {
            self.settings_viewport(&ctx);
            #[cfg(windows)]
            if !self.settings_icon_set {
                self.settings_icon_set = crate::icon::set_window_icons(SETTINGS_TITLE);
            }
        } else {
            self.settings_icon_set = false;
            self.autostart_on = None;
            self.settings_info = None;
        }

        let fading = (full > 0.0 && full < 1.0) || (menu_anim > 0.0 && menu_anim < 1.0);
        let fast = ringing || menu_open || view.holding || bell.hovered() || rejected || fading;
        ctx.request_repaint_after(if fast {
            Duration::from_millis(33)
        } else if self.show_settings {
            // Live status (queue waits, input counters).
            Duration::from_millis(100)
        } else {
            // Idle: nothing on screen moves. The broker, the update checker and
            // the cursor monitor (when the cursor is near) ask for repaints
            // themselves; wake up only for timers that end here.
            let mut wake = IDLE_REPAINT;
            if simplify && !quiet {
                wake = wake.min(Duration::from_secs(after).saturating_sub(now - self.last_active));
            }
            if self.pos_dirty_since.is_some() {
                wake = wake.min(POS_SAVE_DELAY);
            }
            wake
        });
    }
}

// ------------------------------------------------------------------------------------
// Painting helpers
// ------------------------------------------------------------------------------------

/// `full`: 1.0 = normal disc, 0.0 = idle outline look (no disc).
fn paint_disc(p: &Painter, view: &View, ringing: bool, t: f32, hovered: bool, full: f32) {
    if ringing {
        let pulse = (t * 4.0).sin() * 0.5 + 0.5;
        let a = (90.0 + 120.0 * pulse) as u8;
        p.circle_stroke(
            BELL_C,
            DISC_R + 3.0 + 2.0 * pulse,
            Stroke::new(3.0, Color32::from_rgba_unmultiplied(255, 140, 30, a)),
        );
    }
    // An almost invisible backdrop stays behind the outline, so the bell
    // keeps a hover/click target where the OS hit-tests by pixel alpha (macOS).
    p.circle_filled(BELL_C, DISC_R, Color32::from_black_alpha(3));
    let fill = if hovered { 225 } else { 200 };
    p.circle(
        BELL_C,
        DISC_R,
        Color32::from_rgba_unmultiplied(28, 30, 38, fill).gamma_multiply(full),
        Stroke::new(
            1.0,
            Color32::from_rgba_unmultiplied(255, 255, 255, 50).gamma_multiply(full),
        ),
    );
    if let (true, Some(rem)) = (view.holding, view.remaining) {
        let frac = (rem.as_secs_f32() / view.lease.as_secs_f32().max(1.0)).clamp(0.0, 1.0);
        let r = DISC_R + 2.5;
        p.circle_stroke(
            BELL_C,
            r,
            Stroke::new(3.0, Color32::from_rgba_unmultiplied(255, 255, 255, 40)),
        );
        let n = (64.0 * frac).ceil().max(2.0) as usize;
        let start = -std::f32::consts::FRAC_PI_2;
        let pts: Vec<Pos2> = (0..=n)
            .map(|i| {
                let a = start + std::f32::consts::TAU * frac * i as f32 / n as f32;
                BELL_C + r * vec2(a.cos(), a.sin())
            })
            .collect();
        let color = if frac > 0.25 {
            Color32::from_rgb(80, 210, 120)
        } else {
            Color32::from_rgb(240, 150, 40)
        };
        p.add(Shape::line(pts, Stroke::new(3.0, color)));
    }
}

/// Vector bell, `s` = scale in points, `angle` = swing in radians around the top.
fn paint_bell(
    p: &Painter,
    c: Pos2,
    s: f32,
    angle: f32,
    body: Color32,
    shade: Color32,
    opacity: f32,
) {
    let body = body.gamma_multiply(opacity);
    let shade = shade.gamma_multiply(opacity);
    let pivot = c + vec2(0.0, -0.62 * s);
    let rot = |v: Vec2, a: f32| vec2(v.x * a.cos() - v.y * a.sin(), v.x * a.sin() + v.y * a.cos());
    let tf_a = |x: f32, y: f32, a: f32| pivot + rot(c + vec2(x * s, y * s) - pivot, a);
    let tf = |x: f32, y: f32| tf_a(x, y, angle);
    let poly = |pts: &[(f32, f32)], col: Color32| {
        p.add(Shape::convex_polygon(
            pts.iter().map(|&(x, y)| tf(x, y)).collect(),
            col,
            Stroke::NONE,
        ));
    };

    // clapper (swings less than the bell, so it peeks out on the other side)
    p.circle_filled(tf_a(0.0, 0.31, angle * 0.3), 0.10 * s, shade);
    // knob
    p.circle_filled(tf(0.0, -0.64), 0.08 * s, shade);
    // dome
    let dome: Vec<(f32, f32)> = (0..=16)
        .map(|i| {
            let a = std::f32::consts::PI * (1.0 + i as f32 / 16.0);
            (0.34 * a.cos(), -0.22 + 0.36 * a.sin())
        })
        .collect();
    poly(&dome, body);
    // flared body
    poly(
        &[(-0.34, -0.24), (0.34, -0.24), (0.47, 0.24), (-0.47, 0.24)],
        body,
    );
    // rim
    poly(
        &[(-0.56, 0.19), (0.56, 0.19), (0.56, 0.31), (-0.56, 0.31)],
        shade,
    );
    p.circle_filled(tf(-0.56, 0.25), 0.06 * s, shade);
    p.circle_filled(tf(0.56, 0.25), 0.06 * s, shade);
    // highlight
    poly(
        &[(-0.20, -0.40), (-0.12, -0.44), (-0.26, 0.14), (-0.36, 0.14)],
        Color32::from_rgba_unmultiplied(255, 255, 255, 70).gamma_multiply(opacity),
    );
}

fn outline_color(opacity: f32) -> Color32 {
    Color32::from_rgba_unmultiplied(236, 214, 150, 235).gamma_multiply(opacity)
}
/// A soft dark edge under the outline so it reads on light backgrounds too.
fn outline_shadow(opacity: f32) -> Color32 {
    Color32::from_black_alpha(80).gamma_multiply(opacity)
}

/// The bell as a bare line drawing (idle look), same geometry as [`paint_bell`].
fn paint_bell_outline(p: &Painter, c: Pos2, s: f32, opacity: f32) {
    let at = |x: f32, y: f32| c + vec2(x, y) * s;
    let mut body = vec![
        at(-0.56, 0.31),
        at(0.56, 0.31),
        at(0.56, 0.19),
        at(0.455, 0.19),
    ];
    body.extend((0..=16).map(|i| {
        let a = std::f32::consts::TAU - std::f32::consts::PI * i as f32 / 16.0;
        at(0.34 * a.cos(), -0.22 + 0.36 * a.sin())
    }));
    body.extend([at(-0.455, 0.19), at(-0.56, 0.19)]);
    let clapper: Vec<Pos2> = (0..=8)
        .map(|i| {
            let a = std::f32::consts::PI * i as f32 / 8.0;
            at(0.10 * a.cos(), 0.31 + 0.09 * a.sin())
        })
        .collect();
    for (width, color) in [
        (3.4, outline_shadow(opacity)),
        (1.6, outline_color(opacity)),
    ] {
        let st = Stroke::new(width, color);
        p.add(Shape::closed_line(body.clone(), st));
        p.add(Shape::line(clapper.clone(), st));
        p.circle_stroke(at(0.0, -0.655), 0.065 * s, st);
    }
}

/// Hover menu: icons fan out of the bell along an arc down its right side,
/// staggered top (close) to bottom (auto-fire). `anim` runs 0 → 1 on open.
fn paint_menu(
    p: &Painter,
    hover: [bool; MENU_ITEMS],
    anim: f32,
    auto_fire: bool,
    update_ready: bool,
) {
    const STAGGER: f32 = 0.3;
    let span = 1.0 + STAGGER * (MENU_ITEMS - 1) as f32;
    for (i, &hovered) in hover.iter().enumerate() {
        let k = (anim * span - STAGGER * i as f32).clamp(0.0, 1.0);
        if k <= 0.0 {
            continue;
        }
        let e = 1.0 - (1.0 - k).powi(3); // ease-out
        let c = BELL_C + icon_dir(i) * egui::lerp((DISC_R - 4.0)..=MENU_R, e);
        let scale = egui::lerp(0.6..=1.0, e);
        paint_icon(p, i, c, scale, e, hovered, auto_fire);
        if i == SETTINGS && update_ready {
            // An update is ready: a dot on the settings icon.
            p.circle(
                c + vec2(7.5, -7.5) * scale,
                3.5 * scale,
                Color32::from_rgb(80, 150, 255).gamma_multiply(e),
                Stroke::new(1.2, Color32::from_rgb(28, 30, 38).gamma_multiply(e)),
            );
        }
    }
}

fn paint_icon(
    p: &Painter,
    i: usize,
    c: Pos2,
    scale: f32,
    alpha: f32,
    hovered: bool,
    auto_fire: bool,
) {
    let bg = match (i, hovered) {
        (CLOSE, true) => Color32::from_rgb(200, 50, 50),
        (AUTO_FIRE, true) if auto_fire => Color32::from_rgb(250, 160, 50),
        (AUTO_FIRE, false) if auto_fire => Color32::from_rgb(235, 120, 20),
        (_, true) => Color32::from_rgb(70, 110, 200),
        _ => Color32::from_rgba_unmultiplied(28, 30, 38, 220),
    };
    p.circle(
        c,
        ICON_R * scale,
        bg.gamma_multiply(alpha),
        Stroke::new(
            1.0,
            Color32::from_rgba_unmultiplied(255, 255, 255, 60).gamma_multiply(alpha),
        ),
    );
    let white = Color32::from_rgb(235, 235, 235).gamma_multiply(alpha);
    let st = Stroke::new(1.6 * scale, white);
    match i {
        CLOSE => {
            let k = 4.0 * scale;
            p.line_segment([c + vec2(-k, -k), c + vec2(k, k)], st);
            p.line_segment([c + vec2(-k, k), c + vec2(k, -k)], st);
        }
        SETTINGS => {
            p.circle_stroke(c, 3.6 * scale, st);
            for n in 0..8 {
                let a = n as f32 * std::f32::consts::TAU / 8.0;
                let d = vec2(a.cos(), a.sin()) * scale;
                p.line_segment([c + d * 4.6, c + d * 7.0], Stroke::new(2.0 * scale, white));
            }
        }
        MOVE => {
            for d in [
                vec2(1.0, 0.0),
                vec2(-1.0, 0.0),
                vec2(0.0, 1.0),
                vec2(0.0, -1.0),
            ] {
                p.arrow(c, d * 7.0 * scale, Stroke::new(1.4 * scale, white));
            }
        }
        _ => {
            // Auto-fire: a lit flame when on, an unlit outline when off.
            let h = 14.0 * scale;
            if auto_fire {
                paint_flame(
                    p,
                    c,
                    h,
                    Color32::from_rgb(255, 238, 190).gamma_multiply(alpha),
                    Color32::from_rgb(255, 150, 40).gamma_multiply(alpha),
                );
            } else {
                // Unlit: a dim grey flame.
                let grey = Color32::from_rgb(120, 122, 132).gamma_multiply(alpha);
                paint_flame(
                    p,
                    c,
                    h,
                    grey,
                    Color32::from_rgb(70, 72, 82).gamma_multiply(alpha),
                );
            }
        }
    }
}

/// A teardrop (round bottom, pointed `tip`), in units of `h` around `c`.
fn teardrop(c: Pos2, h: f32, tip: Vec2, center: Vec2, r: f32) -> Vec<Pos2> {
    let to_tip = tip - center;
    let half = (r / to_tip.length()).acos();
    let (start, end) = (
        to_tip.angle() + half,
        to_tip.angle() - half + std::f32::consts::TAU,
    );
    std::iter::once(c + tip * h)
        .chain((0..=14).map(|j| {
            let a = start + (end - start) * j as f32 / 14.0;
            c + (center + r * vec2(a.cos(), a.sin())) * h
        }))
        .collect()
}

/// A flame of height `h` centred on `c`: a main tongue leaning left, two
/// smaller side tongues, and an inner core in `inner`.
fn paint_flame(p: &Painter, c: Pos2, h: f32, outer: Color32, inner: Color32) {
    let fill = |pts: Vec<Pos2>, col: Color32| {
        p.add(Shape::convex_polygon(pts, col, Stroke::NONE));
    };
    fill(
        teardrop(c, h, vec2(-0.07, -0.52), vec2(0.0, 0.18), 0.30),
        outer,
    );
    fill(
        teardrop(c, h, vec2(0.31, -0.22), vec2(0.13, 0.17), 0.18),
        outer,
    );
    fill(
        teardrop(c, h, vec2(-0.31, -0.06), vec2(-0.14, 0.21), 0.15),
        outer,
    );
    fill(
        teardrop(c, h, vec2(0.03, -0.10), vec2(0.0, 0.27), 0.15),
        inner,
    );
}

fn paint_auto_fire_badge(p: &Painter, c: Pos2, opacity: f32) {
    p.circle(
        c,
        8.0,
        Color32::from_rgb(235, 120, 20).gamma_multiply(opacity),
        Stroke::new(1.5, Color32::WHITE.gamma_multiply(opacity)),
    );
    paint_flame(
        p,
        c,
        11.0,
        Color32::from_rgb(255, 238, 190).gamma_multiply(opacity),
        Color32::from_rgb(255, 150, 40).gamma_multiply(opacity),
    );
}

/// Speech-bubble colours for the light and dark themes.
struct BubblePalette {
    fill: Color32,
    border: Color32,
    shadow: Color32,
    text: Color32,
    muted: Color32,
    accent: Color32,
    good: Color32,
    warn: Color32,
}

impl BubblePalette {
    fn new(dark: bool) -> Self {
        if dark {
            Self {
                fill: Color32::from_rgb(38, 40, 48),
                border: Color32::from_white_alpha(28),
                shadow: Color32::from_black_alpha(90),
                text: Color32::from_rgb(236, 237, 242),
                muted: Color32::from_rgb(160, 163, 176),
                accent: Color32::from_rgb(125, 175, 255),
                good: Color32::from_rgb(110, 210, 140),
                warn: Color32::from_rgb(245, 170, 80),
            }
        } else {
            Self {
                fill: Color32::from_rgb(253, 253, 251),
                border: Color32::from_black_alpha(36),
                shadow: Color32::from_black_alpha(40),
                text: Color32::from_rgb(28, 30, 36),
                muted: Color32::from_rgb(98, 102, 114),
                accent: Color32::from_rgb(25, 95, 205),
                good: Color32::from_rgb(25, 130, 65),
                warn: Color32::from_rgb(190, 100, 10),
            }
        }
    }
}

fn paint_bubble(p: &Painter, view: &View, dark: bool) {
    let Some(agent) = &view.agent else { return };
    let pal = BubblePalette::new(dark);
    let fmt = |size: f32, color: Color32| TextFormat {
        font_id: FontId::proportional(size),
        color,
        ..Default::default()
    };
    let mut job = LayoutJob::default();
    job.wrap.max_width = 206.0;
    let line = |job: &mut LayoutJob, text: &str, size: f32, color: Color32| {
        if !job.text.is_empty() {
            job.append("\n", 0.0, fmt(size, color));
        }
        job.append(text, 0.0, fmt(size, color));
    };
    // A small vertical gap between blocks.
    let gap = |job: &mut LayoutJob| job.append("\n", 0.0, fmt(5.0, Color32::TRANSPARENT));

    // Who, and what they're doing.
    job.append(agent, 0.0, fmt(14.5, pal.accent));
    job.append(
        if view.holding {
            "  has control"
        } else {
            "  is asking for control"
        },
        0.0,
        fmt(12.0, pal.muted),
    );
    let task: String = if view.task.chars().count() > 140 {
        view.task.chars().take(139).collect::<String>() + "…"
    } else {
        view.task.clone()
    };
    line(&mut job, &task, 13.5, pal.text);
    gap(&mut job);

    // What happens next.
    let take_back = if view.typing_counts {
        "Move the mouse or type to take it back"
    } else {
        "Move the mouse to take it back"
    };
    if view.holding {
        let left = view.remaining.map(|d| d.as_secs()).unwrap_or(0);
        let how = match view.reason {
            Some(GrantReason::Idle) => " · auto-allowed",
            Some(GrantReason::AutoFire) => " · auto-fire",
            _ => "",
        };
        line(&mut job, &format!("{left}s left{how}"), 12.0, pal.good);
        line(&mut job, take_back, 11.5, pal.muted);
    } else {
        line(
            &mut job,
            "Click to allow · right-click to deny",
            11.5,
            pal.muted,
        );
        if let Some(d) = view.auto_in {
            let text = if view.auto_fire {
                format!("Auto-fire: starts in {}s once you're idle", d.as_secs() + 1)
            } else {
                format!("Auto-allows in {}s if you stay idle", d.as_secs())
            };
            line(&mut job, &text, 11.5, pal.warn);
        }
        if view.waiting > 1 {
            line(
                &mut job,
                &format!("+{} more waiting", view.waiting - 1),
                11.5,
                pal.muted,
            );
        }
    }

    let galley = p.layout_job(job);
    let pad = vec2(12.0, 10.0);
    let size = galley.size() + pad * 2.0;
    let right = BELL_C.x - DISC_R - 14.0;
    let bottom = BELL_C.y + 10.0;
    let min = pos2((right - size.x).max(2.0), (bottom - size.y).max(2.0));
    let rect = Rect::from_min_size(min, size);
    let radius = 12.0;

    // soft shadow
    p.rect_filled(
        rect.translate(vec2(0.0, 3.0)).expand(1.0),
        radius + 1.0,
        pal.shadow,
    );
    let border = Stroke::new(1.0, pal.border);
    p.rect(rect, radius, pal.fill, border, StrokeKind::Inside);
    // tail pointing at the bell
    let base_top = pos2(rect.max.x - 1.5, (rect.max.y - 26.0).max(rect.min.y + 6.0));
    let base_bot = pos2(rect.max.x - 1.5, rect.max.y - radius);
    let tip = pos2(BELL_C.x - DISC_R - 2.0, BELL_C.y - 2.0);
    p.add(Shape::convex_polygon(
        vec![base_top, tip, base_bot],
        pal.fill,
        Stroke::NONE,
    ));
    p.line_segment([base_top + vec2(0.5, 0.0), tip], border);
    p.line_segment([base_bot + vec2(0.5, 0.0), tip], border);
    p.galley(rect.min + pad, galley, pal.text);
}

// ------------------------------------------------------------------------------------
// Settings window
// ------------------------------------------------------------------------------------

fn theme_preference(theme: Theme) -> egui::ThemePreference {
    match theme {
        Theme::System => egui::ThemePreference::System,
        Theme::Light => egui::ThemePreference::Light,
        Theme::Dark => egui::ThemePreference::Dark,
    }
}

/// Theme and a slightly roomier, rounder look for the settings window
/// (the bell itself is custom-painted and unaffected).
fn setup_style(ctx: &egui::Context, theme: Theme) {
    ctx.set_theme(theme_preference(theme));
    ctx.all_styles_mut(|s| {
        s.spacing.item_spacing = vec2(8.0, 7.0);
        s.spacing.button_padding = vec2(10.0, 4.0);
        s.spacing.interact_size.y = 22.0;
        let r = egui::CornerRadius::same(6);
        for w in [
            &mut s.visuals.widgets.noninteractive,
            &mut s.visuals.widgets.inactive,
            &mut s.visuals.widgets.hovered,
            &mut s.visuals.widgets.active,
            &mut s.visuals.widgets.open,
        ] {
            w.corner_radius = r;
        }
    });
}

/// iOS-style on/off switch.
fn switch(ui: &mut egui::Ui, on: &mut bool) -> egui::Response {
    let size = ui.spacing().interact_size.y * vec2(1.9, 0.95);
    let (rect, mut resp) = ui.allocate_exact_size(size, Sense::click());
    if resp.clicked() {
        *on = !*on;
        resp.mark_changed();
    }
    if ui.is_rect_visible(rect) {
        let t = ui.ctx().animate_bool_responsive(resp.id, *on);
        let vis = ui.style().interact_selectable(&resp, *on);
        let fill = if *on {
            ui.visuals().selection.bg_fill
        } else {
            vis.bg_fill
        };
        let radius = rect.height() / 2.0;
        ui.painter()
            .rect(rect, radius, fill, vis.bg_stroke, StrokeKind::Inside);
        let x = egui::lerp((rect.left() + radius)..=(rect.right() - radius), t);
        ui.painter().circle(
            pos2(x, rect.center().y),
            radius - 3.0,
            Color32::WHITE,
            Stroke::new(1.0, Color32::from_black_alpha(40)),
        );
    }
    resp
}

/// A switch followed by a (wrapping) label; clicking the label toggles too.
fn switch_row(ui: &mut egui::Ui, on: &mut bool, label: &str) -> egui::Response {
    ui.horizontal(|ui| {
        let mut resp = switch(ui, on);
        let text = ui.add(egui::Label::new(label).wrap().sense(Sense::click()));
        if text.clicked() {
            *on = !*on;
            resp.mark_changed();
        }
        resp | text
    })
    .inner
}

/// A rounded section card with a title.
fn card<R>(ui: &mut egui::Ui, title: &str, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    let r = egui::Frame::new()
        .fill(ui.visuals().faint_bg_color)
        .stroke(ui.visuals().widgets.noninteractive.bg_stroke)
        .corner_radius(10)
        .inner_margin(14)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(egui::RichText::new(title).size(15.0).strong());
            ui.add_space(2.0);
            add(ui)
        })
        .inner;
    ui.add_space(10.0);
    r
}

/// A value shown in a code block, with a button to its right.
fn code_block(ui: &mut egui::Ui, value: &str, add_button: impl FnOnce(&mut egui::Ui)) {
    ui.horizontal_top(|ui| {
        let w = (ui.available_width() - 74.0).max(80.0);
        egui::Frame::new()
            .fill(ui.visuals().extreme_bg_color)
            .corner_radius(6)
            .inner_margin(egui::Margin::symmetric(8, 6))
            .show(ui, |ui| {
                ui.set_width(w - 16.0);
                ui.add(egui::Label::new(egui::RichText::new(value).monospace()).wrap());
            });
        add_button(ui);
    });
}

/// A caption, then the value in a code block with a Copy button.
fn copy_row(ui: &mut egui::Ui, label: &str, value: &str) {
    ui.label(egui::RichText::new(label).weak());
    code_block(ui, value, |ui| {
        let id = Id::unique(("copied", label));
        let now = ui.input(|i| i.time);
        let recently = ui
            .data(|d| d.get_temp::<f64>(id))
            .is_some_and(|t| now - t < 1.5);
        if ui
            .button(if recently { "Copied" } else { "Copy" })
            .clicked()
        {
            ui.ctx().copy_text(value.to_string());
            ui.data_mut(|d| d.insert_temp(id, now));
        }
        if recently {
            ui.ctx().request_repaint_after(Duration::from_millis(300));
        }
    });
    ui.add_space(4.0);
}

/// Show `path` in the platform's file manager (selected, where supported).
fn reveal_in_file_manager(path: &std::path::Path) -> std::io::Result<()> {
    if !path.exists() {
        // Nothing to select yet: open the folder instead.
        let dir = path.parent().unwrap_or(path);
        std::fs::create_dir_all(dir)?;
        return open_path(dir);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        std::process::Command::new("explorer")
            .raw_arg(format!("/select,\"{}\"", path.display()))
            .spawn()
            .map(crate::clients::reap)
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg("-R")
            .arg(path)
            .spawn()
            .map(crate::clients::reap)
    }
    // No portable "select this file" on Linux: open its folder.
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        open_path(path.parent().unwrap_or(path))
    }
}

fn open_path(path: &std::path::Path) -> std::io::Result<()> {
    let cmd = if cfg!(windows) {
        "explorer"
    } else if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    std::process::Command::new(cmd)
        .arg(path)
        .spawn()
        .map(crate::clients::reap)
}

impl BellApp {
    fn settings_viewport(&mut self, ctx: &egui::Context) {
        let builder = egui::ViewportBuilder::default()
            .with_title(SETTINGS_TITLE)
            .with_icon(self.settings_icon.clone())
            .with_inner_size([500.0, 680.0])
            .with_min_inner_size([380.0, 360.0])
            .with_always_on_top();
        ctx.show_viewport_immediate(
            egui::ViewportId::from_hash_of("settings"),
            builder,
            |ui, _class| {
                if ui.ctx().input(|i| i.viewport().close_requested()) {
                    self.show_settings = false;
                }
                // Hardware mode: an agent must not be able to change its own
                // permissions here. Software-generated clicks don't reach the
                // widgets, and keyboard navigation/activation is off (it can't be
                // told apart reliably), so changes only stick with a real click.
                if self.shared.hardware_detection() {
                    let injected_click = ui
                        .ctx()
                        .input(|i| i.pointer.any_pressed() || i.pointer.any_released())
                        && crate::hook::last_button_was_injected();
                    if injected_click {
                        ui.disable();
                    }
                    ui.ctx().input_mut(|i| {
                        i.events.retain(|e| {
                            !matches!(
                                e,
                                egui::Event::Key {
                                    key: egui::Key::Tab | egui::Key::Space | egui::Key::Enter,
                                    ..
                                }
                            )
                        })
                    });
                }
                let bar = egui::Frame::new()
                    .fill(ui.visuals().panel_fill)
                    .inner_margin(egui::Margin::symmetric(16, 12));
                egui::Panel::top("settings-header")
                    .frame(bar)
                    .show(ui, |ui| self.settings_header(ui));
                egui::Panel::bottom("settings-footer")
                    .frame(bar)
                    .show(ui, |ui| self.settings_footer(ui));
                egui::CentralPanel::default()
                    .frame(
                        egui::Frame::new()
                            .fill(ui.visuals().window_fill)
                            .inner_margin(egui::Margin::symmetric(16, 0)),
                    )
                    .show(ui, |ui| {
                        egui::ScrollArea::vertical().show(ui, |ui| {
                            ui.add_space(12.0);
                            self.settings_ui(ui);
                        });
                    });
            },
        );
    }

    fn settings_header(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Deskbell").size(20.0).strong());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let mut theme = self.shared.config.lock().unwrap().theme;
                let before = theme;
                egui::Frame::new()
                    .fill(ui.visuals().extreme_bg_color)
                    .corner_radius(8)
                    .inner_margin(3)
                    .show(ui, |ui| {
                        ui.spacing_mut().item_spacing.x = 2.0;
                        // Right-to-left layout: added in reverse order.
                        ui.selectable_value(&mut theme, Theme::Dark, "Dark");
                        ui.selectable_value(&mut theme, Theme::Light, "Light");
                        ui.selectable_value(&mut theme, Theme::System, "System");
                    });
                if theme != before {
                    self.draft.theme = theme;
                    self.save_config(|c| c.theme = theme);
                    ui.ctx().set_theme(theme_preference(theme));
                }
            });
        });
        ui.weak("Agents ring this bell to ask for your mouse and keyboard.");
    }

    fn settings_footer(&mut self, ui: &mut egui::Ui) {
        let shared = self.shared.clone();
        ui.horizontal(|ui| {
            let dirty = {
                let cfg = shared.config.lock().unwrap();
                Config {
                    window_pos: cfg.window_pos,
                    ..self.draft.clone()
                } != *cfg
            };
            let fill = if dirty {
                ui.visuals().selection.bg_fill
            } else {
                ui.visuals().widgets.inactive.weak_bg_fill
            };
            let save = egui::Button::new(egui::RichText::new("Save").strong()).fill(fill);
            if ui.add_enabled(dirty, save).clicked() {
                let new = self.draft.clone();
                let mut cfg = shared.config.lock().unwrap();
                let pos = cfg.window_pos;
                *cfg = Config {
                    window_pos: pos,
                    ..new
                };
                let msg = match cfg.save() {
                    Ok(()) => "Saved".to_string(),
                    Err(e) => format!("Could not save: {e}"),
                };
                let settings = cfg.broker_settings();
                let auto_update = cfg.auto_update;
                drop(cfg);
                shared.with(|b, now| b.update_settings(settings, now));
                self.updates
                    .send(crate::update::Request::SetAutomatic(auto_update));
                self.saved_msg = Some((msg, Instant::now()));
            }
            if ui.add_enabled(dirty, egui::Button::new("Revert")).clicked() {
                self.draft = shared.config.lock().unwrap().clone();
            }
            if dirty {
                ui.weak("Unsaved changes");
            } else if let Some((msg, at)) = &self.saved_msg
                && at.elapsed() < Duration::from_secs(3)
            {
                ui.label(msg);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Close").clicked() {
                    self.show_settings = false;
                }
            });
        });
    }

    fn settings_ui(&mut self, ui: &mut egui::Ui) {
        let shared = self.shared.clone();
        let info = self
            .settings_info
            .take()
            .unwrap_or_else(|| SettingsInfo::load(self.port));

        // ---- install & startup -------------------------------------------------------
        card(ui, "Install & startup", |ui| {
            use crate::autostart::plain;
            use crate::install::Status;
            match &info.install {
                Status::Installed(path) => {
                    ui.weak(format!("Installed at {}", plain(path)));
                }
                Status::Elsewhere { current, target } => {
                    ui.label(
                        "Not installed yet. Agent configs point at this file, so they break if it's moved or deleted.",
                    );
                    ui.weak(format!(
                        "Running from {}",
                        current.parent().map(plain).unwrap_or_default()
                    ));
                    ui.add_space(2.0);
                    let launcher = if cfg!(windows) {
                        ", adds a Start menu shortcut"
                    } else if cfg!(target_os = "macos") {
                        ""
                    } else {
                        ", adds an app launcher entry"
                    };
                    ui.horizontal_wrapped(|ui| {
                        let install = egui::Button::new(egui::RichText::new("Install").strong())
                            .fill(ui.visuals().selection.bg_fill);
                        if ui.add(install).clicked() {
                            let result = crate::install::install()
                                .and_then(|t| crate::install::relaunch(&t));
                            match result {
                                Ok(()) => ui.ctx().send_viewport_cmd_to(
                                    egui::ViewportId::ROOT,
                                    ViewportCommand::Close,
                                ),
                                Err(e) => self.saved_msg = Some((e, Instant::now())),
                            }
                        }
                        ui.weak(format!(
                            "Copies it to {}{launcher} and restarts from there.",
                            plain(target)
                        ));
                    });
                }
                Status::DevBuild => {
                    ui.weak("Development build: not installable.");
                }
                Status::Unknown => {}
            }
            let mut on = *self
                .autostart_on
                .get_or_insert_with(crate::autostart::is_enabled);
            if switch_row(ui, &mut on, "Start deskbell when I log in").changed() {
                let result = match std::env::current_exe() {
                    Ok(exe) if on => crate::autostart::enable(&exe),
                    Ok(_) => crate::autostart::disable(),
                    Err(e) => Err(e.to_string()),
                };
                if let Err(e) = result {
                    self.saved_msg = Some((e, Instant::now()));
                }
                self.autostart_on = Some(crate::autostart::is_enabled());
            }
        });

        // ---- live status ------------------------------------------------------------
        card(ui, "Status", |ui| {
            if let Some(err) = shared.cursor_error.lock().unwrap().clone() {
                ui.colored_label(Color32::from_rgb(230, 120, 20), format!("Warning: {err}"));
            }
            let (holder, queue) = shared.with(|b, now| {
                (
                    b.holder().map(|l| {
                        (
                            l.agent.clone(),
                            l.task.clone(),
                            l.expires_at.saturating_duration_since(now),
                        )
                    }),
                    b.queue()
                        .iter()
                        .map(|r| {
                            (
                                r.session.clone(),
                                r.agent.clone(),
                                r.task.clone(),
                                now.saturating_duration_since(r.since),
                            )
                        })
                        .collect::<Vec<_>>(),
                )
            });
            match holder {
                Some((agent, task, rem)) => {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(format!(
                            "In control: {agent} - \"{task}\" ({}s left)",
                            rem.as_secs()
                        ));
                        if ui.button("Take back").clicked() {
                            shared.with(|b, now| b.user_revoke(now));
                        }
                    });
                }
                None => {
                    ui.label("Nobody has control: you're in charge.");
                }
            }
            if queue.is_empty() {
                ui.weak("Queue: empty");
            }
            for (i, (session, agent, task, waited)) in queue.iter().enumerate() {
                ui.horizontal_wrapped(|ui| {
                    ui.label(format!(
                        "{}. {agent} - \"{task}\" (waiting {}s)",
                        i + 1,
                        waited.as_secs()
                    ));
                    if ui.small_button("Remove").clicked() {
                        shared.with(|b, now| b.user_remove(session, now));
                    }
                });
            }
        });

        // ---- behaviour --------------------------------------------------------------
        card(ui, "Behaviour", |ui| {
            let d = &mut self.draft;
            switch_row(
                ui,
                &mut d.auto_fire,
                "Auto-fire: allow requests without asking",
            )
            .on_hover_text(
                "Agents still take turns, and moving the mouse still takes control back. \
                     Also available from the bell's hover menu.",
            );
            let mut auto = d.auto_grant_idle_secs > 0;
            ui.horizontal(|ui| {
                switch(ui, &mut auto);
                ui.label("Auto-allow if I'm idle for");
                let mut secs = d.auto_grant_idle_secs.max(5);
                ui.add_enabled(
                    auto,
                    egui::DragValue::new(&mut secs).range(5..=600).suffix(" s"),
                );
                d.auto_grant_idle_secs = if auto { secs } else { 0 };
            });
            switch_row(
                ui,
                &mut d.show_bubble,
                "Show speech bubble with agent and task",
            );
            ui.horizontal(|ui| {
                switch(ui, &mut d.simplify_when_idle);
                ui.label("Fade to a plain outline after");
                ui.add_enabled(
                    d.simplify_when_idle,
                    egui::DragValue::new(&mut d.simplify_after_secs)
                        .range(3..=3600)
                        .suffix(" s"),
                );
                ui.label("of quiet");
            })
            .response
            .on_hover_text("The full bell comes back when you hover it or an agent rings.");
            ui.add_space(4.0);
            egui::Grid::new("behaviour-grid")
                .num_columns(2)
                .spacing([12.0, 8.0])
                .show(ui, |ui| {
                    ui.label("Access lease length");
                    ui.add(
                        egui::DragValue::new(&mut d.lease_secs)
                            .range(10..=600)
                            .suffix(" s"),
                    );
                    ui.end_row();
                    ui.label("Movement that takes control back");
                    ui.add(egui::Slider::new(&mut d.move_threshold_px, 2..=60).suffix(" px"));
                    ui.end_row();
                });
        });

        // ---- input detection --------------------------------------------------------
        let hook_active = shared
            .hook_active
            .load(std::sync::atomic::Ordering::Relaxed);
        card(ui, "How the bell knows it's you", |ui| {
            use crate::config::Detection;
            let d = &mut self.draft;
            ui.add_enabled_ui(hook_active, |ui| {
                ui.radio_value(
                    &mut d.detection,
                    Detection::Hardware,
                    "Only my physical mouse and keyboard count (recommended)",
                );
            });
            ui.indent("hw", |ui| {
                ui.weak(if hook_active {
                    "Agents may use their own mouse/keyboard tools while they hold access. \
                     Software-generated clicks on the bell are ignored, so agents can't approve \
                     themselves. Doesn't suit mouse-sharing, remote-control or voice-control \
                     software, which also generates input."
                } else if cfg!(windows) {
                    "Unavailable: the input hook could not be installed."
                } else {
                    "Not available on this platform yet."
                });
                let enabled = hook_active && d.detection == Detection::Hardware;
                ui.add_enabled_ui(enabled, |ui| {
                    switch_row(
                        ui,
                        &mut d.keyboard_takes_back,
                        "Typing on my keyboard also takes control back",
                    );
                    switch_row(
                        ui,
                        &mut d.unexplained_moves_are_user,
                        "Cursor jumps without any input event count as me",
                    )
                    .on_hover_text(
                        "E.g. SetCursorPos-based tools (pyautogui) or input over elevated (admin) \
                         windows, which the hook can't see. Turn off if your agent's tools use \
                         SetCursorPos.",
                    );
                });
            });
            ui.radio_value(
                &mut d.detection,
                Detection::Movement,
                "Any cursor movement counts, except this server's own tools (typing isn't detected)",
            );
            if hook_active {
                let (hw, inj) = crate::hook::totals();
                ui.weak(format!(
                    "Input seen: {hw} physical, {inj} software-generated events."
                ));
            }
        });

        // ---- connection help --------------------------------------------------------
        card(ui, "Connect your agents", |ui| {
            use crate::clients::Item;
            let clients = &info.clients;
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = vec2(4.0, 4.0);
                for (i, c) in clients.iter().enumerate() {
                    ui.selectable_value(&mut self.client_tab, i, c.name);
                }
            });
            ui.add_space(6.0);
            let client = &clients[self.client_tab.min(clients.len() - 1)];
            for item in &client.items {
                match item {
                    Item::Note(text) => {
                        ui.label(*text);
                        ui.add_space(2.0);
                    }
                    Item::Copy { label, value } => copy_row(ui, label, value),
                    Item::Link { label, url } => {
                        if ui.button(*label).clicked()
                            && let Err(e) = crate::clients::open_url(url)
                        {
                            self.saved_msg = Some((format!("Could not open: {e}"), Instant::now()));
                        }
                        ui.add_space(4.0);
                    }
                    Item::Reveal { label, path } => {
                        let resp = ui.button(*label).on_hover_text(path.display().to_string());
                        if resp.clicked() {
                            let result = match path.parent() {
                                _ if path.exists() => reveal_in_file_manager(path),
                                Some(dir) if dir.exists() => open_path(dir),
                                _ => Err(std::io::Error::other(format!(
                                    "{} not found",
                                    path.display()
                                ))),
                            };
                            if let Err(e) = result {
                                self.saved_msg =
                                    Some((format!("Could not open: {e}"), Instant::now()));
                            }
                        }
                    }
                }
            }
            ui.add_space(4.0);
            ui.separator();
            ui.horizontal(|ui| {
                ui.label("Local port");
                ui.add(egui::DragValue::new(&mut self.draft.port).range(1024..=65535));
                ui.weak("(restart required)");
            });
        });

        // ---- updates ----------------------------------------------------------------
        card(ui, "Updates", |ui| {
            use crate::update::{Request, State};
            let state = self.updates.state();
            ui.horizontal(|ui| {
                ui.label(format!("Version {}", crate::update::CURRENT_VERSION));
                if ui.link("What's new").clicked()
                    && let Err(e) = crate::clients::open_url(&crate::update::release_notes_url())
                {
                    self.saved_msg = Some((format!("Could not open: {e}"), Instant::now()));
                }
            });
            let (good, warn) = if ui.visuals().dark_mode {
                (
                    Color32::from_rgb(110, 210, 140),
                    Color32::from_rgb(245, 170, 80),
                )
            } else {
                (
                    Color32::from_rgb(25, 130, 65),
                    Color32::from_rgb(190, 100, 10),
                )
            };
            match &state {
                State::Idle => {}
                State::Unavailable(why) => {
                    ui.weak(why);
                }
                State::Checking => {
                    ui.label("Checking for updates…");
                }
                State::UpToDate => {
                    ui.colored_label(good, "You're up to date.");
                }
                State::Downloading(v) => {
                    ui.label(format!("Downloading version {v}…"));
                }
                State::Ready(v) => {
                    ui.colored_label(good, format!("Version {v} is ready to install."));
                }
                State::Installing(v) => {
                    ui.label(format!("Installing version {v}…"));
                }
                State::Failed(why) => {
                    ui.colored_label(warn, why);
                }
            }
            let unavailable = matches!(state, State::Unavailable(_));
            let busy = matches!(
                state,
                State::Checking | State::Downloading(_) | State::Installing(_)
            );
            ui.horizontal(|ui| {
                if let State::Ready(v) = &state {
                    let install = egui::Button::new(
                        egui::RichText::new(format!("Restart to update to {v}")).strong(),
                    )
                    .fill(ui.visuals().selection.bg_fill);
                    if ui.add(install).clicked() {
                        self.updates.send(Request::Install);
                    }
                }
                if ui
                    .add_enabled(!busy && !unavailable, egui::Button::new("Check now"))
                    .clicked()
                {
                    self.updates.send(Request::Check);
                }
            });
            if matches!(state, State::Ready(_)) {
                ui.weak("Restarting ends any agent's current access; it can ask again.");
            }
            ui.add_enabled_ui(!unavailable, |ui| {
                switch_row(
                    ui,
                    &mut self.draft.auto_update,
                    "Check for updates automatically (once a day)",
                );
            });
        });

        // ---- files ------------------------------------------------------------------
        if let Some(path) = Config::path() {
            card(ui, "Config file", |ui| {
                code_block(ui, &path.display().to_string(), |ui| {
                    if ui
                        .button("Open")
                        .on_hover_text("Show the file in your file manager")
                        .clicked()
                        && let Err(e) = reveal_in_file_manager(&path)
                    {
                        self.saved_msg = Some((format!("Could not open: {e}"), Instant::now()));
                    }
                });
            });
        }
        self.settings_info = Some(info);
    }
}

/// What the settings window shows about this copy and how to connect agents.
/// It doesn't change while the window is open, so it's looked up when the
/// window opens rather than on every frame.
struct SettingsInfo {
    install: crate::install::Status,
    clients: Vec<crate::clients::Client>,
}

impl SettingsInfo {
    /// `port`: the one the server is actually listening on (a changed port
    /// only applies after a restart).
    fn load(port: u16) -> Self {
        let exe = std::env::current_exe()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| "deskbell".into());
        let url = format!("http://127.0.0.1:{port}/mcp");
        Self {
            install: crate::install::status(),
            clients: crate::clients::all(&exe, &url),
        }
    }
}
