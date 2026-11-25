//! egui application: tabs, widgets, status bar.

use crate::dsp;
use crate::model::*;
use crate::presets;
use crate::profiles::PROFILES;
use crate::tips::{eq_field_tip, tip};
use eframe::egui::{self, Align, Color32, Layout, RichText, Sense, Stroke, Ui, Vec2};
use egui_plot::{Line, Plot, PlotPoints, Points};
use revelator_core::fx;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Meter refresh period while the window is focused / visible but unfocused.
const METER_PERIOD_FOCUSED: Duration = Duration::from_millis(40);
const METER_PERIOD_UNFOCUSED: Duration = Duration::from_millis(100);

const ACCENT: Color32 = Color32::from_rgb(0x4f, 0x9d, 0xff);
const ACCENT_DK: Color32 = Color32::from_rgb(0x2b, 0x68, 0xc2);
const WARN: Color32 = Color32::from_rgb(0xe6, 0xb4, 0x50);
const BAD: Color32 = Color32::from_rgb(0xe0, 0x55, 0x55);
const GOOD: Color32 = Color32::from_rgb(0x4c, 0xc0, 0x76);
const MUTED_TXT: Color32 = Color32::from_rgb(0x8f, 0x96, 0xa5);
const BG: Color32 = Color32::from_rgb(0x12, 0x14, 0x19);
const CARD: Color32 = Color32::from_rgb(0x1b, 0x1e, 0x26);
const CARD_STROKE: Color32 = Color32::from_rgb(0x2e, 0x33, 0x3f);
const BTN: Color32 = Color32::from_rgb(0x2b, 0x30, 0x3c);

/// Fixed column widths of the label / slider / value / badge row grid.
const LABEL_W: f32 = 150.0;
const VALUE_W: f32 = 76.0;
const BADGE_W: f32 = 14.0;
const ROW_H: f32 = 30.0;
const MAX_CONTENT_W: f32 = 1240.0;

/// The core's high-pass has no switch: 24 Hz (its minimum) means off. The UI shows an
/// ON/OFF button and keeps the cut-off to come back to.
const HPF_OFF_HZ: f32 = 24.0;
const HPF_DEFAULT: f32 = 100.0;
const HPF_ID: &str = "dsp.hpf.on";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Tab {
    Mic,
    Mixer,
    Fat,
    Fx,
}

pub struct App {
    model: Model,
    backend: Box<dyn Backend>,
    demo: bool,
    tab: Tab,
    mix_dest: usize,
    eq_drag: Option<usize>,
    /// Cached EQ response: `(inputs, curve)`; recomputed only when the inputs change.
    eq_curve: Option<(Vec<f32>, Vec<[f64; 2]>)>,
    wake_set: bool,
    /// False while minimized; read by the backend wake callback so device updates do not
    /// repaint a hidden window (restoring it repaints and picks up the latest snapshot).
    visible: Arc<AtomicBool>,
    /// Tray icon; while it exists, closing the window hides it instead of quitting.
    pub tray: Option<tray_icon::TrayIcon>,
    /// Window/taskbar icon showing the active preset slot.
    taskbar: Option<Arc<crate::taskbar::Taskbar>>,
    /// Performs jump list / thumbnail button actions (see `attach_taskbar`).
    remote: Option<RemoteFn>,
    /// Actions the UI performs itself, for backends without a remote handle (demo).
    remote_queue: Arc<std::sync::Mutex<Vec<Action>>>,
    /// Action given on the command line of this (first) launch; done once connected.
    pub startup_action: Option<Action>,
    /// Name of the Revelator's plain mic when it is a Windows default input (effects bypass it).
    plain_mic_default: Option<String>,
    plain_mic_polled: Option<Instant>,
    pub hwnd: isize,
    /// Hide the window on the first frame (launched by the "Start on boot" entry).
    pub start_hidden: bool,
    /// Cached "Start on boot" state; re-read from the registry when the window gets focus
    /// (it can also be changed from Task Manager).
    autostart: bool,
    was_focused: bool,
    /// Checks GitHub for a newer release (started by `start_update_checks`).
    updater: Arc<crate::update::Updater>,
    uc_path: String,
    preset_dir: std::path::PathBuf,
    /// Saved user presets (names), refreshed after every change.
    user_presets: Vec<String>,
    new_preset_name: String,
    renaming: Option<Renaming>,
    /// Cut-off the high-pass filter had when it was last on (restored by its ON button).
    hpf_last: f32,
    /// Preset waiting for its delete confirmation.
    confirm_delete: Option<String>,
    notice: Option<(String, bool)>,
    /// Name typed for the next user-preset store (empty keeps the preset's own name).
    user_preset_name: String,
    /// The mic's preset list was requested for this connection.
    presets_asked: Option<Instant>,
    connect_requested: bool,
    /// Peak-hold markers per meter id.
    peaks: HashMap<String, PeakHold>,
}

/// A user preset being renamed inline.
struct Renaming {
    from: String,
    buf: String,
    /// First frame: grab keyboard focus once.
    fresh: bool,
}

/// Held maximum of a meter: stays put for `PEAK_HOLD`, then falls at `PEAK_FALL_DB_S`.
#[derive(Clone, Copy, Debug)]
struct PeakHold {
    value: f32,
    since: Instant,
}

const PEAK_HOLD: Duration = Duration::from_millis(1500);
const PEAK_FALL_DB_S: f32 = 12.0;

impl PeakHold {
    /// Marker value at `now` (never below `floor`).
    fn at(&self, now: Instant, floor: f32) -> f32 {
        let t = now.saturating_duration_since(self.since);
        let fall = t.saturating_sub(PEAK_HOLD).as_secs_f32() * PEAK_FALL_DB_S;
        (self.value - fall).max(floor)
    }
}

fn peak_floor(id: &str) -> f32 {
    if id == "meter.reduction" {
        0.0
    } else {
        DB_FLOOR
    }
}

/// Feed one meter reading into its peak-hold marker. Larger values are "hotter" for both
/// level and gain-reduction meters. Returns the marker value.
fn track_peak(peaks: &mut HashMap<String, PeakHold>, id: &str, v: f32, now: Instant) -> f32 {
    let held = peaks.get(id).map(|p| p.at(now, peak_floor(id)));
    match held {
        Some(h) if h > v => h,
        _ => {
            peaks.insert(
                id.to_string(),
                PeakHold {
                    value: v,
                    since: now,
                },
            );
            v
        }
    }
}

/// `text` shortened with an ellipsis so it fits `max_w` in `font`.
fn fit_text(ui: &Ui, text: &str, font: &egui::FontId, max_w: f32) -> String {
    let width = |t: &str| {
        ui.painter()
            .layout_no_wrap(t.to_string(), font.clone(), Color32::WHITE)
            .size()
            .x
    };
    if width(text) <= max_w {
        return text.to_string();
    }
    let mut chars: Vec<char> = text.chars().collect();
    while !chars.is_empty() {
        chars.pop();
        let t: String = chars.iter().collect::<String>() + "\u{2026}";
        if width(&t) <= max_w {
            return t;
        }
    }
    String::new()
}

/// Frequency axis grid for a plot whose x values are log10(Hz): marks at 1-2-5 steps per
/// decade (labelled), with fainter lines at the other whole numbers.
fn freq_grid(input: egui_plot::GridInput) -> Vec<egui_plot::GridMark> {
    let (lo, hi) = input.bounds;
    let mut marks = Vec::new();
    for dec in (lo.floor() as i32 - 1)..=(hi.ceil() as i32) {
        for m in 1..10 {
            let value = (m as f64 * 10f64.powi(dec)).log10();
            if value < lo || value > hi {
                continue;
            }
            let step_size = match m {
                1 => 1.0,
                2 | 5 => 0.3,
                _ => 0.05,
            };
            marks.push(egui_plot::GridMark { value, step_size });
        }
    }
    marks
}

pub fn setup_style(ctx: &egui::Context) {
    let mut v = egui::Visuals::dark();
    v.panel_fill = BG;
    v.window_fill = CARD;
    v.extreme_bg_color = Color32::from_rgb(0x0d, 0x0f, 0x13);
    v.faint_bg_color = Color32::from_rgb(0x17, 0x1a, 0x20);
    v.selection.bg_fill = ACCENT_DK;
    v.selection.stroke = Stroke::new(1.0_f32, Color32::WHITE);
    v.hyperlink_color = ACCENT;
    v.override_text_color = Some(Color32::from_gray(228));
    let r = egui::Rounding::same(6.0);
    v.widgets.noninteractive.rounding = r;
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0_f32, CARD_STROKE);
    v.widgets.inactive.rounding = r;
    v.widgets.inactive.bg_fill = BTN;
    v.widgets.inactive.weak_bg_fill = BTN;
    v.widgets.inactive.bg_stroke = Stroke::new(1.0_f32, Color32::from_rgb(0x3a, 0x40, 0x4e));
    v.widgets.hovered.rounding = r;
    v.widgets.hovered.bg_fill = Color32::from_rgb(0x38, 0x3f, 0x50);
    v.widgets.hovered.weak_bg_fill = Color32::from_rgb(0x38, 0x3f, 0x50);
    v.widgets.hovered.bg_stroke = Stroke::new(1.0_f32, ACCENT);
    v.widgets.active.rounding = r;
    v.widgets.active.bg_fill = ACCENT_DK;
    v.widgets.active.weak_bg_fill = ACCENT_DK;
    v.widgets.open.rounding = r;
    ctx.set_visuals(v);
    ctx.style_mut(|s| {
        // Static text is not selectable (it feels odd in a control panel); the few labels
        // worth copying (errors, device name) opt back in with `Label::selectable(true)`.
        s.interaction.selectable_labels = false;
        s.spacing.item_spacing = Vec2::new(10.0, 8.0);
        s.spacing.button_padding = Vec2::new(12.0, 6.0);
        s.spacing.interact_size = Vec2::new(40.0, 30.0);
        s.spacing.slider_rail_height = 6.0;
        s.spacing.combo_width = 120.0;
        use egui::{FontId, TextStyle};
        s.text_styles
            .insert(TextStyle::Body, FontId::proportional(14.5));
        s.text_styles
            .insert(TextStyle::Button, FontId::proportional(14.5));
        s.text_styles
            .insert(TextStyle::Small, FontId::proportional(12.0));
        s.text_styles
            .insert(TextStyle::Monospace, FontId::monospace(13.5));
        s.text_styles
            .insert(TextStyle::Heading, FontId::proportional(21.0));
    });
}

impl App {
    /// Make the window/taskbar icon and title, and the tray icon and tooltip, follow the active
    /// preset slot, also while minimized or hidden, and set up the jump list and thumbnail
    /// buttons. Call on the main thread after creating the tray icon.
    pub fn attach_taskbar(&mut self, hwnd: isize, ctx: &egui::Context) {
        let remote = self.backend.remote().unwrap_or_else(|| {
            let (q, c) = (self.remote_queue.clone(), ctx.clone());
            Arc::new(move |a| {
                q.lock().unwrap_or_else(|e| e.into_inner()).push(a);
                c.request_repaint();
            })
        });
        self.remote = Some(remote.clone());
        let t = Arc::new(crate::taskbar::Taskbar::new(
            hwnd,
            self.tray.clone(),
            remote,
        ));
        let h = t.clone();
        self.backend
            .set_status_hook(Arc::new(move |st| h.set(crate::taskbar::status_of(st))));
        self.taskbar = Some(t);
    }

    pub fn new(mut backend: Box<dyn Backend>, demo: bool) -> Self {
        backend.connect();
        let uc_default = std::env::var("APPDATA")
            .map(|a| format!("{a}\\Fender\\Revelator Series\\Revelator Dynamic\\presets\\scene\\Default Mix.scene"))
            .unwrap_or_default();
        let preset_dir = presets::default_dir();
        App {
            model: Model::default(),
            backend,
            demo,
            tab: Tab::Mic,
            mix_dest: 0,
            eq_drag: None,
            eq_curve: None,
            wake_set: false,
            visible: Arc::new(AtomicBool::new(true)),
            tray: None,
            taskbar: None,
            remote: None,
            remote_queue: Arc::default(),
            startup_action: None,
            plain_mic_default: None,
            plain_mic_polled: None,
            hwnd: 0,
            start_hidden: false,
            autostart: crate::autostart::is_enabled(),
            was_focused: true,
            updater: crate::update::Updater::new(),
            uc_path: uc_default,
            user_presets: presets::list(&preset_dir),
            preset_dir,
            new_preset_name: String::new(),
            renaming: None,
            hpf_last: HPF_DEFAULT,
            confirm_delete: None,
            notice: None,
            user_preset_name: String::new(),
            presets_asked: None,
            connect_requested: true,
            peaks: HashMap::new(),
        }
    }

    /// Start the background check for a newer release (not in demo mode or tests).
    pub fn start_update_checks(&self, ctx: &egui::Context) {
        let c = ctx.clone();
        self.updater.spawn(move || c.request_repaint());
    }

    /// Select the initial tab by name (`mic|mixer|fat|fx`); unknown names are ignored.
    pub fn select_tab(&mut self, name: &str) {
        self.tab = match name.to_ascii_lowercase().as_str() {
            "mic" => Tab::Mic,
            "mixer" => Tab::Mixer,
            "fat" => Tab::Fat,
            "fx" => Tab::Fx,
            _ => self.tab,
        };
    }

    fn error(&mut self, msg: impl Into<String>) {
        self.model.state.last_error = Some(msg.into());
    }

    /// The control can reach the device (always true in demo mode). Controls without a known
    /// device address are shown disabled and never sent.
    fn can_send(&self, id: &str) -> bool {
        self.demo || crate::map::hw_supported(id)
    }

    fn set(&mut self, id: &str, v: f32) {
        if !self.can_send(id) {
            return;
        }
        match self.model.set_local(id, v, Instant::now()) {
            Ok(v) => {
                if let Err(e) = self.backend.set(id, v) {
                    self.error(format!("{id}: {e}"));
                }
            }
            Err(e) => self.error(e),
        }
    }

    // ------------------------------------------------------------ widgets

    /// Fixed-width badge cell: a dot for non-verified parameters, blank otherwise.
    fn conf_marker(&self, ui: &mut Ui, id: &str) {
        let (rect, resp) = ui.allocate_exact_size(Vec2::new(BADGE_W, ROW_H), Sense::hover());
        let Some(s) = spec(id) else { return };
        let (col, tip, hollow) = match s.confidence {
            Confidence::Verified => return,
            Confidence::Inferred => (
                WARN,
                "Parameter mapping is inferred; not yet verified on hardware.",
                false,
            ),
            Confidence::Unknown => (
                BAD,
                "Parameter mapping is a guess; not yet verified on hardware.",
                true,
            ),
        };
        let c = rect.center();
        if hollow {
            ui.painter()
                .circle_stroke(c, 4.0, Stroke::new(1.5_f32, col));
        } else {
            ui.painter().circle_filled(c, 4.0, col);
        }
        resp.on_hover_text(tip);
    }

    /// Label cell of fixed width, left aligned, truncated with a tooltip.
    fn label_cell(ui: &mut Ui, text: &str) {
        Self::label_cell_tip(ui, text, None);
    }

    /// Like `label_cell`, with an explanation as tooltip. A truncated label already shows its
    /// full text on hover, so labels with an explanation are never truncated (one tooltip only).
    fn label_cell_tip(ui: &mut Ui, text: &str, tip: Option<&str>) {
        ui.allocate_ui_with_layout(
            Vec2::new(LABEL_W, ROW_H),
            Layout::left_to_right(Align::Center),
            |ui| {
                ui.set_min_size(Vec2::new(LABEL_W, ROW_H));
                let font = egui::TextStyle::Body.resolve(ui.style());
                let fits = ui
                    .painter()
                    .layout_no_wrap(text.to_string(), font, Color32::WHITE)
                    .size()
                    .x
                    <= LABEL_W;
                match tip {
                    // Fits: our explanation is the only tooltip.
                    Some(t) if fits => {
                        ui.add(egui::Label::new(text)).on_hover_text(t);
                    }
                    // Too long: egui already shows the full text on hover.
                    _ => {
                        ui.add(egui::Label::new(text).truncate());
                    }
                }
            },
        );
    }

    /// Slider width that makes label + slider + value + badge fill the available row.
    fn slider_w(ui: &Ui) -> f32 {
        let sp = ui.spacing().item_spacing.x;
        (ui.available_width() - LABEL_W - VALUE_W - BADGE_W - 3.0 * sp).clamp(80.0, 520.0)
    }

    fn slider(&mut self, ui: &mut Ui, id: &str, label: Option<&str>) {
        self.slider_ex(ui, id, label, None);
    }

    /// Slider + value box; `step` snaps the value (e.g. 1.0 for integer settings).
    fn slider_ex(&mut self, ui: &mut Ui, id: &str, label: Option<&str>, step: Option<f32>) {
        let Some(s) = spec(id) else { return };
        let mut v = self.model.get(id);
        // The high-pass minimum means "off", which has its own button.
        let lo = if id == "dsp.hpf.freq" { 30.0 } else { s.min };
        let sw = Self::slider_w(ui);
        let mut changed = false;
        let enabled = self.can_send(id);
        ui.horizontal(|ui| {
            if !enabled {
                ui.disable();
            }
            ui.spacing_mut().slider_width = sw;
            Self::label_cell_tip(ui, label.unwrap_or(&s.label), tip(id));
            let mut w = egui::Slider::new(&mut v, lo..=s.max).show_value(false);
            if s.max / s.min.max(0.0001) > 100.0 && s.min > 0.0 {
                w = w.logarithmic(true);
            }
            if let Some(st) = step {
                w = w.step_by(st as f64);
            }
            let resp = ui.add(w);
            if let Some(t) = tip(id) {
                resp.clone().on_hover_text(t);
            }
            changed |= resp.changed();
            // Small ranges (0..1 amounts, ms attack times) need more decimals.
            let decimals = if step.is_some() {
                0
            } else if s.max - s.min <= 2.0 || s.min.abs() < 0.1 && s.min > 0.0 {
                2
            } else {
                1
            };
            let dv = egui::DragValue::new(&mut v)
                .range(lo..=s.max)
                .speed(step.unwrap_or(((s.max - s.min) / 300.0).max(0.01)))
                .suffix(s.unit)
                .max_decimals(decimals);
            changed |= ui.add_sized([VALUE_W, ROW_H - 2.0], dv).changed();
            self.conf_marker(ui, id);
        });
        if changed {
            self.set(id, v);
        }
    }

    fn onoff(&mut self, ui: &mut Ui, id: &str) {
        self.onoff_with(ui, id, tip(id).unwrap_or("Enable / disable"));
    }

    /// ON state of a switch. The high-pass switch is derived from its frequency.
    fn is_on(&self, id: &str) -> bool {
        if id == HPF_ID {
            self.model.get("dsp.hpf.freq") > HPF_OFF_HZ + 0.5
        } else {
            self.model.on(id)
        }
    }

    fn set_switch(&mut self, id: &str, on: bool) {
        if id == HPF_ID {
            let f = if on { self.hpf_last } else { HPF_OFF_HZ };
            self.set("dsp.hpf.freq", f);
        } else {
            self.set(id, if on { 1.0 } else { 0.0 });
        }
    }

    fn onoff_with(&mut self, ui: &mut Ui, id: &str, tooltip: &str) {
        let on = self.is_on(id);
        let b = egui::Button::new(
            RichText::new(if on { "ON" } else { "OFF" })
                .strong()
                .size(12.5)
                .color(if on { Color32::WHITE } else { MUTED_TXT }),
        )
        .fill(if on { ACCENT_DK } else { BTN })
        .min_size(Vec2::new(50.0, 24.0));
        if ui.add(b).on_hover_text(tooltip).clicked() {
            self.set_switch(id, !on);
        }
    }

    fn choice(&mut self, ui: &mut Ui, id: &str, label: &str, opts: &[&str]) {
        let cur = (self.model.get(id) as usize).min(opts.len() - 1);
        let enabled = self.can_send(id);
        let cw = Self::slider_w(ui);
        ui.horizontal(|ui| {
            if !enabled {
                ui.disable();
            }
            Self::label_cell_tip(ui, label, tip(id));
            let combo = egui::ComboBox::from_id_salt(id)
                .width(cw)
                .selected_text(opts[cur])
                .show_ui(ui, |ui| {
                    for (i, o) in opts.iter().enumerate() {
                        if ui.selectable_label(i == cur, *o).clicked() {
                            self.set(id, i as f32);
                        }
                    }
                });
            if let Some(t) = tip(id) {
                combo.response.on_hover_text(t);
            }
            self.conf_marker(ui, id);
        });
    }

    /// Titled card spanning the available width, with an optional ON/OFF switch in the header.
    fn card(
        &mut self,
        ui: &mut Ui,
        title: &str,
        on: Option<&str>,
        body: impl FnOnce(&mut Self, &mut Ui),
    ) {
        egui::Frame::none()
            .fill(CARD)
            .stroke(Stroke::new(1.0_f32, CARD_STROKE))
            .rounding(8.0)
            .inner_margin(14.0)
            .show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.add(
                        egui::Label::new(RichText::new(title).size(16.5).strong().color(ACCENT))
                            .truncate(),
                    );
                    if let Some(id) = on {
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            self.onoff(ui, id);
                        });
                    }
                });
                ui.add_space(2.0);
                // A switched-off section stays visible but dimmed.
                let off = on.is_some_and(|id| !self.is_on(id));
                ui.scope(|ui| {
                    if off {
                        ui.set_opacity(0.45);
                    }
                    body(self, ui);
                });
            });
    }

    /// Two cards side by side when there is room, stacked otherwise.
    fn two(
        &mut self,
        ui: &mut Ui,
        a: impl FnOnce(&mut Self, &mut Ui),
        b: impl FnOnce(&mut Self, &mut Ui),
    ) {
        if ui.available_width() >= 900.0 {
            ui.columns(2, |c| {
                a(self, &mut c[0]);
                b(self, &mut c[1]);
            });
        } else {
            a(self, ui);
            b(self, ui);
        }
    }

    /// Segmented meter. `reduction` shows 0..30 dB gain reduction instead of a level.
    /// `peak` is the held maximum, drawn as a thin bright marker.
    fn meter(
        ui: &mut Ui,
        db: f32,
        peak: f32,
        size: Vec2,
        vertical: bool,
        reduction: bool,
        label: &str,
    ) {
        let (rect, resp) = ui.allocate_exact_size(size, Sense::hover());
        let p = ui.painter();
        p.rect_filled(rect, 3.0, Color32::from_rgb(0x0b, 0x0c, 0x10));
        let to_frac = |v: f32| {
            if reduction {
                (v / 30.0).clamp(0.0, 1.0)
            } else {
                meter_fraction(v)
            }
        };
        let frac = to_frac(db);
        let len = if vertical { size.y } else { size.x };
        let n = ((len / 6.0) as usize).clamp(10, 60);
        let gap = 1.5;
        let inner = rect.shrink(2.0);
        let seg = ((if vertical {
            inner.height()
        } else {
            inner.width()
        }) - gap * (n as f32 - 1.0))
            / n as f32;
        for i in 0..n {
            let lo = i as f32 / n as f32;
            let hi = (i + 1) as f32 / n as f32;
            let level_db = -60.0 + hi * 60.0;
            let col = if reduction {
                WARN
            } else if level_db > -3.0 {
                BAD
            } else if level_db > -12.0 {
                WARN
            } else {
                GOOD
            };
            let lit = frac >= (lo + hi) * 0.5 && frac > 0.0;
            let col = if lit { col } else { col.linear_multiply(0.16) };
            let off = i as f32 * (seg + gap);
            let r = if vertical {
                egui::Rect::from_min_size(
                    egui::pos2(inner.left(), inner.bottom() - off - seg),
                    Vec2::new(inner.width(), seg),
                )
            } else {
                egui::Rect::from_min_size(
                    egui::pos2(inner.left() + off, inner.top()),
                    Vec2::new(seg, inner.height()),
                )
            };
            p.rect_filled(r, 1.0, col);
        }
        let pf = to_frac(peak);
        if pf > 0.0 {
            let col = if reduction {
                WARN
            } else if peak > -3.0 {
                BAD
            } else if peak > -12.0 {
                WARN
            } else {
                GOOD
            };
            let col = col.lerp_to_gamma(Color32::WHITE, 0.45);
            let marker = if vertical {
                let y = (inner.bottom() - pf * inner.height()).max(inner.top() + 1.0);
                egui::Rect::from_min_max(
                    egui::pos2(inner.left(), y - 1.0),
                    egui::pos2(inner.right(), y + 1.0),
                )
            } else {
                let x = (inner.left() + pf * inner.width()).min(inner.right() - 1.0);
                egui::Rect::from_min_max(
                    egui::pos2(x - 1.0, inner.top()),
                    egui::pos2(x + 1.0, inner.bottom()),
                )
            };
            p.rect_filled(marker, 0.0, col);
        }
        let fmt = |v: f32| {
            if !reduction && v <= DB_FLOOR {
                "-inf dB".to_string()
            } else {
                format!("{v:.1} dB")
            }
        };
        resp.on_hover_text(format!("{label}: {}  (peak {})", fmt(db), fmt(peak)));
    }

    /// dB scale labels under a horizontal level meter of width `w`.
    fn meter_scale(ui: &mut Ui, w: f32, reduction: bool) {
        let (rect, _) = ui.allocate_exact_size(Vec2::new(w, 13.0), Sense::hover());
        let ticks: &[(f32, &str)] = if reduction {
            &[(0.0, "0"), (10.0, "10"), (20.0, "20"), (30.0, "30 dB")]
        } else {
            &[
                (-60.0, "-60"),
                (-48.0, "-48"),
                (-36.0, "-36"),
                (-24.0, "-24"),
                (-12.0, "-12"),
                (0.0, "0 dB"),
            ]
        };
        let font = egui::FontId::proportional(10.5);
        for (v, t) in ticks {
            let f = if reduction {
                v / 30.0
            } else {
                meter_fraction(*v)
            };
            let x = rect.left() + 2.0 + f * (w - 4.0);
            let (anchor, x) = if f <= 0.01 {
                (egui::Align2::LEFT_TOP, rect.left())
            } else if f >= 0.99 {
                (egui::Align2::RIGHT_TOP, rect.right())
            } else {
                (egui::Align2::CENTER_TOP, x)
            };
            ui.painter().text(
                egui::pos2(x, rect.top()),
                anchor,
                t,
                font.clone(),
                MUTED_TXT,
            );
        }
    }

    /// Held peak for meter `id` (fed each frame by `update_peaks`).
    fn peak(&self, id: &str, db: f32) -> f32 {
        self.peaks
            .get(id)
            .map_or(db, |p| p.at(Instant::now(), peak_floor(id)).max(db))
    }

    /// Feed the latest meter readings into the peak-hold markers. Returns true while a marker
    /// is still above its meter (so it needs repaints to fall).
    fn update_peaks(&mut self, now: Instant) -> bool {
        let mut falling = false;
        for (id, &v) in &self.model.state.meters {
            falling |= track_peak(&mut self.peaks, id, v, now) > v + 0.05;
        }
        falling
    }

    /// Labelled horizontal meter row: name, bar (with scale beneath) and numeric readout.
    fn meter_row(&self, ui: &mut Ui, label: &str, id: &str, reduction: bool) {
        let db = self.model.meter(id);
        let peak = self.peak(id, db);
        let sp = ui.spacing().item_spacing.x;
        let w = (ui.available_width() - LABEL_W - VALUE_W - 2.0 * sp).clamp(120.0, 460.0);
        ui.horizontal_top(|ui| {
            Self::label_cell(ui, label);
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 2.0;
                Self::meter(ui, db, peak, Vec2::new(w, 16.0), false, reduction, label);
                Self::meter_scale(ui, w, reduction);
            });
            let txt = if reduction {
                format!("-{db:.1} dB")
            } else if db <= DB_FLOOR {
                "-inf dB".to_string()
            } else {
                format!("{db:.1} dB")
            };
            ui.allocate_ui_with_layout(
                Vec2::new(VALUE_W, 18.0),
                Layout::right_to_left(Align::Center),
                |ui| {
                    ui.set_min_width(VALUE_W);
                    ui.label(RichText::new(txt).monospace().color(MUTED_TXT));
                },
            );
        });
    }

    // ------------------------------------------------------------ tabs

    fn mic_tab(&mut self, ui: &mut Ui) {
        self.two(
            ui,
            |s, ui| {
                s.card(ui, "Preamp & monitoring", None, |s, ui| {
                    s.slider(ui, "mic.gain", None);
                    s.slider(ui, "mic.hp_volume", None);
                    s.slider(ui, "mic.monitor_blend", Some("Monitor blend"));
                    ui.horizontal(|ui| {
                        ui.add_space(LABEL_W + ui.spacing().item_spacing.x);
                        ui.label(
                            RichText::new("0 % = mic only, 100 % = playback only")
                                .small()
                                .color(MUTED_TXT),
                        );
                    });
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        Self::label_cell(ui, "Microphone");
                        let muted = s.model.on("mic.mute");
                        let btn = egui::Button::new(
                            RichText::new(if muted { "MUTED" } else { "Mute" })
                                .strong()
                                .size(16.0)
                                .color(Color32::WHITE),
                        )
                        .fill(if muted { BAD } else { BTN })
                        .min_size(Vec2::new(150.0, 38.0));
                        if ui
                            .add(btn)
                            .on_hover_text(if muted {
                                "Click to unmute the microphone"
                            } else {
                                "Click to mute the microphone"
                            })
                            .clicked()
                        {
                            s.set("mic.mute", if muted { 0.0 } else { 1.0 });
                        }
                        s.conf_marker(ui, "mic.mute");
                    });
                    ui.add_space(4.0);
                    s.meter_row(ui, "Mic level", "meter.mic", false);
                    s.meter_row(ui, "Gain reduction", "meter.reduction", true);
                });
            },
            |s, ui| {
                s.card(ui, "Preset slots", None, |s, ui| {
                    let active = s.model.get("mic.slot") as usize;
                    let sp = ui.spacing().item_spacing.x;
                    let w = ((ui.available_width() - sp * (SLOTS as f32 - 1.0)) / SLOTS as f32)
                        .clamp(40.0, 110.0);
                    ui.horizontal(|ui| {
                        for i in 0..SLOTS {
                            let sel = i == active;
                            let (rect, r) =
                                ui.allocate_exact_size(Vec2::new(w, 58.0), Sense::click());
                            let vis = ui.style().interact_selectable(&r, sel);
                            ui.painter()
                                .rect(rect, vis.rounding, vis.bg_fill, vis.bg_stroke);
                            // the slot's colour, as on the taskbar icon
                            let [cr, cg, cb] =
                                crate::icon::SLOT_COLORS[i.min(crate::icon::SLOTS - 1)];
                            let bar = egui::Rect::from_min_size(
                                rect.left_top() + Vec2::new(8.0, 4.0),
                                Vec2::new(rect.width() - 16.0, 3.0),
                            );
                            ui.painter().rect_filled(
                                bar,
                                1.5,
                                Color32::from_rgb(cr, cg, cb)
                                    .gamma_multiply(if sel { 1.0 } else { 0.45 }),
                            );
                            let fg = if sel {
                                Color32::WHITE
                            } else {
                                Color32::from_gray(228)
                            };
                            let held = s
                                .model
                                .state
                                .mic_presets
                                .as_ref()
                                .and_then(|h| h.slots.get(i).cloned())
                                .unwrap_or_default();
                            let name = held.trim();
                            let (num_y, num_anchor) = if name.is_empty() {
                                (rect.center().y, egui::Align2::CENTER_CENTER)
                            } else {
                                (rect.top() + 20.0, egui::Align2::CENTER_CENTER)
                            };
                            ui.painter().text(
                                egui::pos2(rect.center().x, num_y),
                                num_anchor,
                                format!("{}", i + 1),
                                egui::FontId::proportional(20.0),
                                fg,
                            );
                            if !name.is_empty() {
                                let font = egui::FontId::proportional(12.0);
                                let shown = fit_text(ui, name, &font, w - 12.0);
                                ui.painter().text(
                                    egui::pos2(rect.center().x, rect.bottom() - 15.0),
                                    egui::Align2::CENTER_CENTER,
                                    shown,
                                    font,
                                    if sel { fg } else { MUTED_TXT },
                                );
                            }
                            let r = r.on_hover_text(
                                "Switch the mic to this slot. Choose what a slot holds in \"Mic presets\" below.",
                            );
                            if r.clicked() {
                                s.recall(i);
                            }
                        }
                    });
                    s.hint(
                        ui,
                        "Picks which slot the mic uses (same as its preset button). Mixer levels and Voice FX are not part of a mic preset: keep those in \"My presets\" below.",
                    );
                    ui.add_space(4.0);
                    s.choice(
                        ui,
                        "mic.preset_button_mode",
                        "Preset button",
                        &["Single", "Double", "Multi"],
                    );
                    s.choice(
                        ui,
                        "mic.mute_button_mode",
                        "Mute button",
                        &["Mutes headphones", "Mutes mic"],
                    );
                });
            },
        );
        ui.add_space(2.0);
        self.mic_presets_card(ui);
        ui.add_space(2.0);
        self.user_presets_card(ui);
        ui.add_space(2.0);
        self.uc_import_card(ui);
    }

    fn mixer_tab(&mut self, ui: &mut Ui) {
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("Send to").color(MUTED_TXT));
            for (i, (_, name)) in DESTS.iter().enumerate() {
                let b = egui::Button::new(*name)
                    .selected(self.mix_dest == i)
                    .min_size(Vec2::new(72.0, 28.0));
                if ui.add(b).clicked() {
                    self.mix_dest = i;
                }
            }
            self.conf_marker(ui, "mixer.mic.main");
            ui.label(
                RichText::new("Faders show each channel's level into the selected mix.")
                    .small()
                    .color(MUTED_TXT),
            );
        });
        ui.add_space(6.0);
        let dest = DESTS[self.mix_dest].0;
        let fader_h = (ui.ctx().screen_rect().height() - 300.0).clamp(130.0, 380.0);
        egui::ScrollArea::horizontal()
            .id_salt("strips")
            .auto_shrink([false, true])
            .show(ui, |ui| {
                ui.horizontal_top(|ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    for (ch, name) in CHANNELS {
                        let audible = self.model.channel_audible(ch);
                        let id = mixer_id(ch, dest);
                        self.strip(
                            ui,
                            name,
                            &id,
                            &format!("meter.ch.{ch}"),
                            ch,
                            fader_h,
                            audible,
                        );
                    }
                    ui.add_space(24.0);
                    for (d, name) in DESTS {
                        self.strip(
                            ui,
                            &format!("{name} out"),
                            &out_id(d, "vol"),
                            &format!("meter.out.{d}"),
                            &format!("out.{d}"),
                            fader_h,
                            true,
                        );
                    }
                });
            });
    }

    /// One vertical fader strip. Mute/solo ids derive from `key` ("mic" or "out.main").
    #[allow(clippy::too_many_arguments)]
    fn strip(
        &mut self,
        ui: &mut Ui,
        name: &str,
        fader_id: &str,
        meter_id: &str,
        key: &str,
        fader_h: f32,
        audible: bool,
    ) {
        let Some(s) = spec(fader_id) else { return };
        const INNER_W: f32 = 80.0;
        let mut v = self.model.get(fader_id);
        egui::Frame::none()
            .fill(CARD)
            .stroke(Stroke::new(1.0_f32, CARD_STROKE))
            .rounding(8.0)
            .inner_margin(egui::Margin::symmetric(10.0, 10.0))
            .show(ui, |ui| {
                ui.vertical(|ui| {
                    ui.set_width(INNER_W);
                    ui.set_opacity(if audible { 1.0 } else { 0.5 });
                    ui.spacing_mut().item_spacing.y = 6.0;
                    ui.add_sized(
                        [INNER_W, 20.0],
                        egui::Label::new(RichText::new(name).strong().color(ACCENT)).truncate(),
                    )
                    .on_hover_text(name);
                    ui.horizontal(|ui| {
                        ui.spacing_mut().slider_width = fader_h;
                        ui.spacing_mut().item_spacing.x = 10.0;
                        ui.add_space(14.0);
                        let w = egui::Slider::new(&mut v, s.min..=s.max)
                            .vertical()
                            .show_value(false);
                        let resp = ui.add(w);
                        // Slider's own response can swallow double-clicks (it senses drags),
                        // so check the raw pointer input over the fader's rect as well.
                        let dbl = resp.double_clicked()
                            || (ui.input(|i| {
                                i.pointer
                                    .button_double_clicked(egui::PointerButton::Primary)
                            }) && ui.rect_contains_pointer(resp.rect));
                        if dbl {
                            // Double-click resets the fader to unity gain (0 dB).
                            v = 0.0_f32.clamp(s.min, s.max);
                            self.set(fader_id, v);
                        } else if resp.changed() {
                            self.set(fader_id, v);
                        }
                        let db = self.model.meter(meter_id);
                        Self::meter(
                            ui,
                            db,
                            self.peak(meter_id, db),
                            Vec2::new(14.0, fader_h),
                            true,
                            false,
                            "Level",
                        );
                    });
                    let txt = if v <= DB_FLOOR + 0.5 {
                        "-inf".to_string()
                    } else {
                        format!("{v:+.1} dB")
                    };
                    ui.add_sized(
                        [INNER_W, 18.0],
                        egui::Label::new(RichText::new(txt).monospace()),
                    );
                    let (mute_id, solo_id) = match key.strip_prefix("out.") {
                        Some(d) => (out_id(d, "mute"), None),
                        None => (mixer_id(key, "mute"), Some(mixer_id(key, "solo"))),
                    };
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 6.0;
                        let bw = if solo_id.is_some() { 37.0 } else { INNER_W };
                        let m = self.model.on(&mute_id);
                        let mb =
                            egui::Button::new(RichText::new("M").strong().color(Color32::WHITE))
                                .fill(if m { BAD } else { BTN })
                                .min_size(Vec2::new(bw, 26.0));
                        if ui.add(mb).on_hover_text("Mute").clicked() {
                            self.set(&mute_id, if m { 0.0 } else { 1.0 });
                        }
                        if let Some(sid) = solo_id {
                            let so = self.model.on(&sid);
                            let sb = egui::Button::new(RichText::new("S").strong().color(if so {
                                Color32::BLACK
                            } else {
                                Color32::WHITE
                            }))
                            .fill(if so { WARN } else { BTN })
                            .min_size(Vec2::new(bw, 26.0));
                            if ui.add(sb).on_hover_text("Solo").clicked() {
                                self.set(&sid, if so { 0.0 } else { 1.0 });
                            }
                        }
                    });
                });
            });
    }

    fn fat_tab(&mut self, ui: &mut Ui) {
        self.card(ui, "Fat Channel", None, |s, ui| {
            s.toggle(ui, "mixer.mic.bypass", "Bypass Fat Channel");
            s.slider(ui, "mixer.mic.dspamount", Some("Fat Channel amount"));
            s.hint(
                ui,
                "Bypass turns the whole Fat Channel (gate, compressor, EQ, limiter) off without losing its settings. Keep the amount at 1 for normal use.",
            );
        });
        ui.add_space(6.0);
        self.card(ui, "Profiles", None, |s, ui| {
            let mut chosen = None;
            ui.horizontal_wrapped(|ui| {
                for (i, p) in PROFILES.iter().enumerate() {
                    let active = p.matches(&s.model);
                    let b = egui::Button::new(
                        RichText::new(format!("{}  {}", p.icon, p.name)).strong(),
                    )
                    .selected(active)
                    .min_size(Vec2::new(130.0, 32.0));
                    let r = ui.add(b).on_hover_text(p.blurb);
                    if r.clicked() {
                        chosen = Some(i);
                    }
                }
            });
            ui.add_space(4.0);
            s.hint(
                ui,
                "A profile only changes the live settings: it is not stored anywhere. To keep a setup (a profile or your own tweaks), save it as a preset in \"My presets\" on the Mic tab.",
            );
            s.hint(
                ui,
                "Changes are sent to the mic as you make them. The mic cannot report its DSP settings, so the app shows the values it last sent.",
            );
            if let Some(i) = chosen {
                let n = s.apply_values(PROFILES[i].values().to_vec());
                s.notice = Some((
                    format!("Applied profile \"{}\" ({n} values)", PROFILES[i].name),
                    false,
                ));
            }
        });
        ui.add_space(2.0);
        self.card(ui, "Equalizer", Some("dsp.eq.on"), |s, ui| {
            let model = s.model.get("dsp.eq.model").round() as usize;
            s.choice(ui, "dsp.eq.model", "Model", &dsp::EQ_MODELS);
            let w = ui.available_width();
            let h = (w * 0.22).clamp(190.0, 300.0);
            s.eq_plot(ui, h);
            ui.label(
                RichText::new(if model == 0 {
                    "Curve of the filters sent to the mic (high-pass included). Drag a node to set frequency and gain; scroll over a node to change Q."
                } else {
                    "Curve of the filters sent to the mic (high-pass included)."
                })
                .small()
                .color(MUTED_TXT),
            );
            ui.add_space(2.0);
            match model {
                1 => s.passive_eq(ui),
                2 => s.vintage_eq(ui),
                _ => s.parametric_eq(ui, w),
            }
            ui.add_space(4.0);
            s.toggle(ui, "dsp.eq.swap", "Swap EQ/comp order");
        });
        ui.add_space(2.0);
        self.two(
            ui,
            |s, ui| {
                s.card(ui, "High-pass filter", Some(HPF_ID), |s, ui| {
                    let f = s.model.get("dsp.hpf.freq");
                    if f > HPF_OFF_HZ + 0.5 {
                        s.hpf_last = f;
                    }
                    s.slider(ui, "dsp.hpf.freq", Some("Frequency"));
                });
                s.card(ui, "Gate / expander", Some("dsp.gate.on"), |s, ui| {
                    s.toggle(ui, "dsp.gate.expander", "Expander mode");
                    for id in [
                        "dsp.gate.threshold",
                        "dsp.gate.range",
                        "dsp.gate.attack",
                        "dsp.gate.release",
                        "dsp.gate.keyfilter",
                    ] {
                        s.slider(ui, id, None);
                    }
                    s.toggle(ui, "dsp.gate.keylisten", "Key listen");
                });
            },
            |s, ui| {
                s.card(ui, "Compressor", Some("dsp.comp.on"), |s, ui| {
                    s.choice(ui, "dsp.comp.model", "Model", &dsp::COMP_MODELS);
                    match s.model.get("dsp.comp.model").round() as i32 {
                        1 => {
                            s.choice(ui, "dsp.comp.tube.mode", "Mode", &["Compress", "Limit"]);
                            s.slider(ui, "dsp.comp.tube.gain", None);
                            s.slider(ui, "dsp.comp.tube.peak", None);
                        }
                        2 => {
                            for id in [
                                "dsp.comp.fet.input",
                                "dsp.comp.fet.output",
                                "dsp.comp.fet.attack",
                                "dsp.comp.fet.release",
                            ] {
                                s.slider(ui, id, None);
                            }
                            s.choice(
                                ui,
                                "dsp.comp.fet.ratio",
                                "Ratio",
                                &["4:1", "8:1", "12:1", "20:1", "All buttons"],
                            );
                        }
                        _ => {
                            for id in [
                                "dsp.comp.threshold",
                                "dsp.comp.ratio",
                                "dsp.comp.attack",
                                "dsp.comp.release",
                                "dsp.comp.gain",
                            ] {
                                s.slider(ui, id, None);
                            }
                            s.toggle(ui, "dsp.comp.softknee", "Soft knee");
                            s.toggle(ui, "dsp.comp.auto", "Auto attack/release");
                        }
                    }
                    s.slider(ui, "dsp.comp.keyfilter", None);
                    s.toggle(ui, "dsp.comp.keylisten", "Key listen");
                    s.meter_row(ui, "Reduction", "meter.reduction", true);
                });
                s.card(ui, "Limiter", Some("dsp.limiter.on"), |s, ui| {
                    s.slider(ui, "dsp.limiter.threshold", Some("Threshold"));
                });
            },
        );
    }

    /// Muted one-line note.
    fn hint(&self, ui: &mut Ui, text: &str) {
        ui.label(RichText::new(text).small().color(MUTED_TXT));
    }

    /// Labelled ON/OFF row.
    fn toggle(&mut self, ui: &mut Ui, id: &str, label: &str) {
        ui.horizontal(|ui| {
            Self::label_cell_tip(ui, label, tip(id));
            self.onoff_with(ui, id, "Enable / disable");
            self.conf_marker(ui, id);
        });
    }

    fn parametric_eq(&mut self, ui: &mut Ui, w: f32) {
        let per_row = if w >= 700.0 { 4 } else { 2 };
        for chunk in 0..(4 / per_row) {
            ui.columns(per_row, |cols| {
                for (i, col) in cols.iter_mut().enumerate() {
                    let (band, name) = dsp::EQ_BANDS[chunk * per_row + i];
                    egui::Frame::none()
                        .fill(Color32::from_rgb(0x15, 0x18, 0x1e))
                        .rounding(6.0)
                        .inner_margin(10.0)
                        .show(col, |ui| {
                            ui.set_min_width(ui.available_width());
                            ui.horizontal(|ui| {
                                ui.add(egui::Label::new(RichText::new(name).strong()).truncate());
                                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                    self.onoff(ui, &format!("dsp.eq.{band}.on"));
                                });
                            });
                            if band == "low" || band == "high" {
                                let id = format!("dsp.eq.{band}.shelf");
                                let shelf = self.model.on(&id);
                                ui.horizontal(|ui| {
                                    for (v, l) in [(false, "Peak"), (true, "Shelf")] {
                                        if ui.selectable_label(shelf == v, l).clicked() {
                                            self.set(&id, if v { 1.0 } else { 0.0 });
                                        }
                                    }
                                });
                            } else {
                                ui.label(RichText::new("Peak").color(MUTED_TXT));
                            }
                            for (f, l) in [("freq", "Freq"), ("gain", "Gain"), ("q", "Q")] {
                                let id = format!("dsp.eq.{band}.{f}");
                                let Some(sp) = spec(&id) else { continue };
                                let mut v = self.model.get(&id);
                                let mut changed = false;
                                ui.horizontal(|ui| {
                                    ui.allocate_ui_with_layout(
                                        Vec2::new(36.0, ROW_H),
                                        Layout::left_to_right(Align::Center),
                                        |ui| {
                                            ui.set_min_size(Vec2::new(36.0, ROW_H));
                                            ui.label(RichText::new(l).color(MUTED_TXT))
                                                .on_hover_text(eq_field_tip(f));
                                        },
                                    );
                                    let mut dv = egui::DragValue::new(&mut v)
                                        .range(sp.min..=sp.max)
                                        .suffix(sp.unit);
                                    dv = if f == "freq" {
                                        dv.speed(5.0).max_decimals(0)
                                    } else {
                                        dv.speed(0.05).max_decimals(2)
                                    };
                                    let dw = (ui.available_width() - BADGE_W - 10.0).max(50.0);
                                    changed |= ui.add_sized([dw, ROW_H - 2.0], dv).changed();
                                    self.conf_marker(ui, &id);
                                });
                                if changed {
                                    self.set(&id, v);
                                }
                            }
                        });
                }
            });
        }
    }

    fn passive_eq(&mut self, ui: &mut Ui) {
        self.two(
            ui,
            |s, ui| {
                ui.label(RichText::new("Low").strong());
                s.choice(
                    ui,
                    "dsp.eq.passive.lowfreq",
                    "Frequency",
                    &["20 Hz", "30 Hz", "60 Hz", "100 Hz"],
                );
                s.slider(ui, "dsp.eq.passive.lowboost", Some("Boost"));
                s.slider(ui, "dsp.eq.passive.lowatten", Some("Attenuation"));
            },
            |s, ui| {
                ui.label(RichText::new("High").strong());
                s.choice(
                    ui,
                    "dsp.eq.passive.highfreq",
                    "Boost frequency",
                    &[
                        "3 kHz", "4 kHz", "5 kHz", "8 kHz", "10 kHz", "12 kHz", "16 kHz",
                    ],
                );
                s.slider(ui, "dsp.eq.passive.highboost", Some("Boost"));
                s.slider(ui, "dsp.eq.passive.bandwidth", Some("Bandwidth"));
                s.choice(
                    ui,
                    "dsp.eq.passive.attenfreq",
                    "Atten. frequency",
                    &["5 kHz", "10 kHz", "20 kHz"],
                );
                s.slider(ui, "dsp.eq.passive.highatten", Some("Attenuation"));
            },
        );
    }

    fn vintage_eq(&mut self, ui: &mut Ui) {
        self.two(
            ui,
            |s, ui| {
                ui.label(RichText::new("Low").strong());
                s.choice(
                    ui,
                    "dsp.eq.vintage.lowfreq",
                    "Frequency",
                    &["35 Hz", "60 Hz", "110 Hz", "220 Hz"],
                );
                s.slider(ui, "dsp.eq.vintage.lowgain", Some("Gain"));
                ui.label(RichText::new("Low-mid").strong());
                s.choice(
                    ui,
                    "dsp.eq.vintage.lowmidfreq",
                    "Frequency",
                    &["360 Hz", "700 Hz", "1.6 kHz"],
                );
                s.slider(ui, "dsp.eq.vintage.lowmidgain", Some("Gain"));
            },
            |s, ui| {
                ui.label(RichText::new("High-mid").strong());
                s.choice(
                    ui,
                    "dsp.eq.vintage.himidfreq",
                    "Frequency",
                    &["3.2 kHz", "4.8 kHz", "7.2 kHz"],
                );
                s.slider(ui, "dsp.eq.vintage.himidgain", Some("Gain"));
                ui.label(RichText::new("High").strong());
                s.slider(ui, "dsp.eq.vintage.highgain", Some("Gain"));
            },
        );
    }

    fn eq_plot(&mut self, ui: &mut Ui, height: f32) {
        let fs = self.model.get("sys.samplerate").max(8000.0);
        let parametric = self.model.get("dsp.eq.model").round() as i32 == 0;
        let st = dsp::state_from(|id| self.model.get(id));
        let mut key: Vec<f32> = fx::PARAMS
            .iter()
            .filter(|p| p.key.starts_with("eq.") || p.key == "hpf.freq")
            .map(|p| st.get(p.key))
            .collect();
        key.push(fs);
        if self.eq_curve.as_ref().is_none_or(|(k, _)| *k != key) {
            let mut writes = st.writes(fx::Engine::Eq, fs);
            writes.extend(st.writes(fx::Engine::Hpf, fs));
            let pts = (0..=240)
                .map(|i| {
                    let lx =
                        20f64.log10() + (i as f64 / 240.0) * (20000f64.log10() - 20f64.log10());
                    let f = 10f64.powf(lx) as f32;
                    let db = fx::response_db(&writes, fs, f).clamp(-30.0, 30.0);
                    [lx, db as f64]
                })
                .collect();
            self.eq_curve = Some((key, pts));
        }
        let pts: Vec<[f64; 2]> = self
            .eq_curve
            .as_ref()
            .map(|(_, p)| p.clone())
            .unwrap_or_default();
        let nodes: Vec<[f64; 2]> = if parametric {
            dsp::EQ_BANDS
                .iter()
                .map(|(b, _)| {
                    [
                        self.model.get(&format!("dsp.eq.{b}.freq")).log10() as f64,
                        self.model.get(&format!("dsp.eq.{b}.gain")) as f64,
                    ]
                })
                .collect()
        } else {
            Vec::new()
        };
        let mut new_vals: Vec<(String, f32)> = Vec::new();
        let mut drag = self.eq_drag;
        // Drawn the same when the EQ is off: the card dims as a whole instead.
        let color = ACCENT;
        Plot::new("eq_plot")
            .height(height)
            .allow_drag(false)
            .allow_zoom(false)
            .allow_scroll(false)
            .allow_boxed_zoom(false)
            .include_x(20f64.log10())
            .include_x(20000f64.log10())
            .include_y(-16.0)
            .include_y(16.0)
            // No cursor: no crosshair and no value tooltip.
            .show_x(false)
            .show_y(false)
            .label_formatter(|_, _| String::new())
            .x_grid_spacer(freq_grid)
            .x_axis_formatter(|m, _| {
                let f = 10f64.powf(m.value);
                if f >= 1000.0 {
                    format!("{:.0}k", f / 1000.0)
                } else {
                    format!("{f:.0}")
                }
            })
            .y_axis_formatter(|m, _| format!("{:+.0}", m.value))
            .show(ui, |pu| {
                pu.line(
                    Line::new(PlotPoints::from(vec![
                        [20f64.log10(), 0.0],
                        [20000f64.log10(), 0.0],
                    ]))
                    .color(Color32::from_gray(80)),
                );
                pu.line(Line::new(PlotPoints::from(pts)).color(color).width(2.5_f32));
                if nodes.is_empty() {
                    return;
                }
                pu.points(
                    Points::new(nodes.clone())
                        .radius(7.0_f32)
                        .color(color)
                        .filled(true),
                );
                let resp = pu.response().clone();
                let ptr = pu.pointer_coordinate();
                let near = ptr.and_then(|p| {
                    let ps = pu.transform().position_from_point(&p);
                    nodes
                        .iter()
                        .enumerate()
                        .map(|(i, n)| {
                            (
                                i,
                                pu.transform()
                                    .position_from_point(&egui_plot::PlotPoint::new(n[0], n[1]))
                                    .distance(ps),
                            )
                        })
                        .filter(|(_, d)| *d < 16.0)
                        .min_by(|a, b| a.1.total_cmp(&b.1))
                        .map(|(i, _)| i)
                });
                if resp.drag_started() {
                    drag = near;
                }
                if !resp.dragged() && !resp.drag_started() {
                    drag = None;
                }
                if let (Some(i), Some(p), true) = (drag, ptr, resp.dragged()) {
                    let b = dsp::EQ_BANDS[i].0;
                    new_vals.push((format!("dsp.eq.{b}.freq"), 10f64.powf(p.x) as f32));
                    new_vals.push((format!("dsp.eq.{b}.gain"), p.y as f32));
                }
                if let (Some(i), true) = (near, resp.hovered()) {
                    let scroll = pu.ctx().input(|i| i.raw_scroll_delta.y);
                    if scroll != 0.0 {
                        let b = dsp::EQ_BANDS[i].0;
                        let q = self.model.get(&format!("dsp.eq.{b}.q"));
                        new_vals.push((format!("dsp.eq.{b}.q"), q * (1.0 + scroll * 0.003)));
                    }
                }
            });
        self.eq_drag = drag;
        for (id, v) in new_vals {
            self.set(&id, v);
        }
    }

    fn fx_tab(&mut self, ui: &mut Ui) {
        let sr = self.model.get("sys.samplerate");
        let rate_ok = (self.demo || self.model.known("sys.samplerate")) && !delay_blocked(sr);
        self.fx_tab_cards(ui, rate_ok);
    }

    /// Warn when Windows records from the Revelator's plain mic: Voice FX and reverb live in the
    /// mic's mixer, so only the Main and Stream Mix outputs carry them.
    fn plain_mic_warning(&mut self, ui: &mut Ui) {
        const EVERY: Duration = Duration::from_secs(2);
        if !self.demo && self.plain_mic_polled.is_none_or(|t| t.elapsed() >= EVERY) {
            self.plain_mic_polled = Some(Instant::now());
            self.plain_mic_default = revelator_usb::audio_rate::plain_mic_default(
                &revelator_usb::audio_rate::default_capture(),
            );
        }
        ui.ctx().request_repaint_after(EVERY);
        if let Some(name) = &self.plain_mic_default {
            ui.label(
                RichText::new(format!(
                    "\u{26A0} Windows records from \"{name}\". Voice FX and reverb are only in the Stream Mix inputs (and the headphones), not in the plain mic. In Discord or your recording app, pick \"Stream Mix A\" as the input."
                ))
                .color(WARN),
            );
            ui.add_space(6.0);
        }
    }

    fn fx_tab_cards(&mut self, ui: &mut Ui, rate_ok: bool) {
        self.plain_mic_warning(ui);
        self.card(ui, "USB outputs", None, |s, ui| {
            s.toggle(ui, "mixer.mic.dawpost", "Fat Channel on USB Stream Outputs");
            s.hint(
                ui,
                "On (the default): what you record is what you hear in the headphones, with the Fat Channel preset applied. Off: recordings get the raw, unprocessed mic while you still hear the processed sound. This does not move the reverb into the plain Mic input: the reverb is a mixer channel, so pick a Stream Mix input for it.",
            );
        });
        ui.add_space(6.0);
        self.two(
            ui,
            |s, ui| {
                s.card(ui, "Voice FX", Some("dsp.voicefx.on"), |s, ui| {
                    let cur = (s.model.get(ECHO_ID).round().max(0.0) as usize)
                        .min(dsp::VOICEFX_MODELS.len() - 1);
                    ui.horizontal(|ui| {
                        Self::label_cell_tip(ui, "Effect", tip("dsp.voicefx.model"));
                        egui::ComboBox::from_id_salt("voicefx_model")
                            .width(170.0)
                            .selected_text(dsp::VOICEFX_MODELS[cur])
                            .show_ui(ui, |ui| {
                                for (i, name) in dsp::VOICEFX_MODELS.iter().enumerate() {
                                    let echo = i as f32 == dsp::ECHO_MODEL;
                                    let r = ui.add_enabled(
                                        !(echo && !rate_ok),
                                        egui::SelectableLabel::new(i == cur, *name),
                                    );
                                    let r = if echo && !rate_ok {
                                        r.on_disabled_hover_text(
                                            "Pick 44.1 or 48 kHz in the status bar first: Echo (Delay) at 88.2/96 kHz crashes the mic firmware into its bootloader.",
                                        )
                                    } else {
                                        r
                                    };
                                    if r.clicked() {
                                        s.set(ECHO_ID, i as f32);
                                    }
                                }
                            });
                        s.conf_marker(ui, ECHO_ID);
                    });
                    if !rate_ok {
                        s.hint(
                            ui,
                            "Echo needs a known sample rate of 44.1 or 48 kHz (status bar).",
                        );
                    } else if cur as f32 == dsp::ECHO_MODEL {
                        s.hint(
                            ui,
                            "Choose another effect before switching Windows to 88.2/96 kHz: Echo at those rates crashes the mic firmware. The app switches Echo off when it sees the change, but only after Windows has made it.",
                        );
                    }
                    s.slider(ui, "dsp.voicefx.mix", Some("Wet/dry"));
                    let ids: &[(&str, &str)] = match cur {
                        1 => &[("detune.amount", "Amount")],
                        2 => &[
                            ("vocoder.volume", "Volume"),
                            ("vocoder.carrierfreq", "Carrier frequency"),
                        ],
                        3 => &[
                            ("ringmod.freq", "Frequency"),
                            ("ringmod.subfreq", "Sub frequency"),
                            ("ringmod.dist", "Distortion"),
                            ("ringmod.volume", "Volume"),
                        ],
                        4 => &[
                            ("robot.pitch", "Pitch"),
                            ("robot.regen", "Regeneration"),
                            ("robot.damping", "Damping"),
                            ("robot.dist", "Distortion"),
                            ("robot.volume", "Volume"),
                        ],
                        5 => &[("echo.time", "Time"), ("echo.feedback", "Feedback")],
                        _ => &[("doubler.lows", "Lows"), ("doubler.width", "Width")],
                    };
                    if cur == 2 {
                        s.choice(
                            ui,
                            "dsp.voicefx.vocoder.carrier",
                            "Carrier",
                            &["Carrier 1", "Carrier 2", "Carrier 3"],
                        );
                    }
                    if cur == 3 {
                        s.toggle(ui, "dsp.voicefx.ringmod.sub", "Sub carrier");
                    }
                    for (k, l) in ids {
                        let step = (*k == "detune.amount").then_some(1.0);
                        s.slider_ex(ui, &format!("dsp.voicefx.{k}"), Some(l), step);
                    }
                });
            },
            |s, ui| {
                s.card(ui, "Reverb", Some("dsp.reverb.on"), |s, ui| {
                    s.slider(ui, "dsp.reverb.size", Some("Size"));
                    s.slider(ui, "dsp.reverb.mix", Some("Wet/dry"));
                    s.slider(ui, "dsp.reverb.hpf", Some("High-pass"));
                    s.slider(ui, "dsp.reverb.predelay", Some("Pre-delay"));
                    s.slider(ui, "mixer.mic.reverb", Some("Reverb send"));
                    s.hint(
                        ui,
                        "Reverb level is the mic's send into the reverb. Like Universal Control, it follows the mic's mute, volume and the monitor blend. You hear the reverb through the Reverb strip in the Mixer tab.",
                    );
                });
            },
        );
    }

    fn recall(&mut self, slot: usize) {
        match self.backend.recall_slot(slot) {
            Ok(()) => self.notice = Some((format!("Recalled slot {}", slot + 1), false)),
            Err(e) => self.error(format!("recall slot {}: {e}", slot + 1)),
        }
    }

    fn load_slot(&mut self, slot: usize) {
        if let Err(e) = self.backend.load_slot(slot) {
            self.error(format!("load slot {}: {e}", slot + 1));
        }
    }

    fn apply_values(&mut self, vals: Vec<(ParamId, f32)>) -> usize {
        let n = vals.len();
        for (id, v) in vals {
            // Sample rate is a device setting, never part of a scene.
            if id != "sys.samplerate" {
                self.set(&id, v);
            }
        }
        n
    }

    fn refresh_presets(&mut self) {
        self.user_presets = presets::list(&self.preset_dir);
    }

    fn save_preset(&mut self) {
        let name = presets::clean_name(&self.new_preset_name);
        let existed = presets::exists(&self.preset_dir, &name);
        let text = scene_to_json(&self.model.state.values);
        match presets::save(&self.preset_dir, &name, &text) {
            Ok(n) => {
                let verb = if existed { "Updated" } else { "Saved" };
                self.notice = Some((format!("{verb} preset \"{n}\""), false));
                self.new_preset_name.clear();
            }
            Err(e) => self.error(format!("save preset: {e}")),
        }
        self.refresh_presets();
    }

    /// Apply one of the mic's eight factory presets (their DSP settings) to the app.
    fn load_factory(&mut self, i: usize) {
        let (name, bytes) = revelator_core::presetdoc::FACTORY[i];
        let Ok(doc) = revelator_core::ubjson::decode(bytes) else {
            self.error(format!("factory preset \"{name}\" is unreadable"));
            return;
        };
        let mut vals = Vec::new();
        for (key, v) in revelator_core::presetdoc::values(&doc) {
            if let Some(p) = revelator_core::fx::find(&key) {
                vals.push((crate::dsp::id(&key), crate::dsp::to_gui(p, v)));
            }
        }
        let n = self.apply_values(vals);
        self.notice = Some((
            format!("Loaded factory preset \"{name}\" ({n} values)"),
            false,
        ));
    }

    fn load_preset(&mut self, name: &str) {
        match presets::read(&self.preset_dir, name).and_then(|t| scene_from_json(&t)) {
            Ok(v) => {
                let n = self.apply_values(v);
                self.notice = Some((format!("Loaded preset \"{name}\" ({n} values)"), false));
            }
            Err(e) => self.error(format!("load preset \"{name}\": {e}")),
        }
    }

    fn assign(&mut self, slot: usize, src: PresetSource) {
        if let Err(e) = self.backend.assign_slot(slot, src) {
            self.error(format!("assign to slot {}: {e}", slot + 1));
        }
    }

    /// The mic's own presets: 8 factory ones (read-only), 8 user ones (writable) and the four
    /// slots the physical button cycles through, with one-click assignment.
    fn mic_presets_card(&mut self, ui: &mut Ui) {
        let connected = self.model.state.connection == Connection::Connected;
        if !connected {
            self.presets_asked = None;
        } else {
            const EVERY: Duration = Duration::from_secs(10);
            let due = self.presets_asked.is_none_or(|t| t.elapsed() >= EVERY);
            if due {
                self.presets_asked = Some(Instant::now());
                // the demo backend has nothing to read: ignore its refusal
                let _ = self.backend.refresh_mic_presets();
            }
            ui.ctx().request_repaint_after(EVERY);
        }
        let held = self.model.state.mic_presets.clone();
        let slot_name = |i: usize| {
            held.as_ref()
                .and_then(|h| h.slots.get(i))
                .cloned()
                .unwrap_or_else(|| "...".into())
        };
        let user_name = |i: usize| {
            held.as_ref()
                .and_then(|h| h.user.get(i))
                .cloned()
                .unwrap_or_else(|| "...".into())
        };
        self.card(ui, "Mic presets and button slots", None, |s, ui| {
            ui.label(RichText::new("Button slots").strong());
            s.hint(
                ui,
                "The four presets the mic's physical button chooses between. Pick which preset each slot holds; the mic keeps its own copy.",
            );
            for i in 0..SLOTS {
                ui.horizontal(|ui| {
                    ui.label(format!("Slot {}", i + 1));
                    egui::ComboBox::from_id_salt(("assign_slot", i))
                        .width(240.0)
                        .selected_text(slot_name(i))
                        .show_ui(ui, |ui| {
                            ui.label(RichText::new("Factory").small().color(MUTED_TXT));
                            for (k, (name, _)) in
                                revelator_core::presetdoc::FACTORY.iter().enumerate()
                            {
                                if ui.selectable_label(false, *name).clicked() {
                                    s.assign(i, PresetSource::Factory(k));
                                }
                            }
                            ui.separator();
                            ui.label(RichText::new("My mic presets").small().color(MUTED_TXT));
                            for k in 0..8 {
                                let n = user_name(k);
                                if !MicPresets::defined(&n) {
                                    continue;
                                }
                                if ui
                                    .selectable_label(false, format!("User {}: {n}", k + 1))
                                    .clicked()
                                {
                                    s.assign(i, PresetSource::User(k));
                                }
                            }
                        });
                    if ui
                        .button("Load")
                        .on_hover_text("Apply this slot's EQ, compressor, gate, limiter and high-pass settings in the app")
                        .clicked()
                    {
                        s.load_slot(i);
                    }
                });
            }
            ui.add_space(4.0);
            egui::CollapsingHeader::new("All mic presets (8 factory, 8 yours)")
                .show(ui, |ui| {
                    s.hint(
                        ui,
                        "Factory presets are read-only. Yours can be overwritten with the app's current settings. Use the menu on a row to load it or put it in a slot.",
                    );
                    let factory = revelator_core::presetdoc::FACTORY;
                    for k in 0..16usize {
                        let user = k >= 8;
                        let name = if user {
                            user_name(k - 8)
                        } else {
                            factory.get(k).map_or(String::new(), |f| f.0.to_string())
                        };
                        ui.horizontal(|ui| {
                            let w = (ui.available_width() - 90.0).max(120.0);
                            ui.add_sized(
                                [w, 20.0],
                                egui::Label::new(if user {
                                    RichText::new(format!("Yours {}: {name}", k - 7))
                                } else {
                                    RichText::new(format!("Factory: {name}"))
                                })
                                .truncate(),
                            );
                            ui.menu_button("Actions", |ui| {
                                if !user && ui.button("Load into the app").clicked() {
                                    s.load_factory(k);
                                    ui.close_menu();
                                }
                                if !user || MicPresets::defined(&name) {
                                    ui.label(RichText::new("Put in slot").small().color(MUTED_TXT));
                                    ui.horizontal(|ui| {
                                        for i in 0..SLOTS {
                                            if ui.button(format!("{}", i + 1)).clicked() {
                                                let src = if user {
                                                    PresetSource::User(k - 8)
                                                } else {
                                                    PresetSource::Factory(k)
                                                };
                                                s.assign(i, src);
                                                ui.close_menu();
                                            }
                                        }
                                    });
                                }
                                if user {
                                    if MicPresets::defined(&name) {
                                        ui.separator();
                                    }
                                    ui.add(
                                        egui::TextEdit::singleline(&mut s.user_preset_name)
                                            .char_limit(revelator_core::presetdoc::MAX_NAME)
                                            .hint_text(
                                                RichText::new("New name (optional)")
                                                    .italics()
                                                    .color(Color32::from_gray(105)),
                                            )
                                            .desired_width(160.0),
                                    );
                                    if ui
                                        .button(if MicPresets::defined(&name) {
                                            "Overwrite with current settings"
                                        } else {
                                            "Store current settings here"
                                        })
                                        .on_hover_text("EQ, compressor, gate, limiter and high-pass; the old contents are backed up")
                                        .clicked()
                                    {
                                        let n = s.user_preset_name.clone();
                                        if let Err(e) = s.backend.save_user_preset(k - 8, &n) {
                                            s.error(format!("store user preset {}: {e}", k - 7));
                                        }
                                        ui.close_menu();
                                    }
                                    if MicPresets::defined(&name)
                                        && ui
                                            .button(RichText::new("Delete this preset").color(BAD))
                                            .on_hover_text("Empties it in the mic; the old contents are backed up")
                                            .clicked()
                                    {
                                        if let Err(e) = s.backend.delete_user_preset(k - 8) {
                                            s.error(format!("delete user preset {}: {e}", k - 7));
                                        }
                                        ui.close_menu();
                                    }
                                }
                            });
                        });
                    }
                });
        });
    }

    /// Saved user presets: save the current settings under a name, load, rename, delete.
    fn user_presets_card(&mut self, ui: &mut Ui) {
        self.card(ui, "My presets", None, |s, ui| {
            ui.horizontal(|ui| {
                Self::label_cell(ui, "Save current as");
                let w =
                    (ui.available_width() - 90.0 - ui.spacing().item_spacing.x).clamp(100.0, 420.0);
                let r = ui.add(
                    egui::TextEdit::singleline(&mut s.new_preset_name)
                        .hint_text(
                            RichText::new("Preset name")
                                .italics()
                                .color(Color32::from_gray(105)),
                        )
                        .desired_width(w),
                );
                let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                let ok = !presets::clean_name(&s.new_preset_name).is_empty();
                let save = ui
                    .add_enabled(
                        ok,
                        egui::Button::new("Save").min_size(Vec2::new(70.0, 28.0)),
                    )
                    .on_hover_text("Saves mixer levels, mic settings and all DSP settings")
                    .clicked();
                if (save || enter) && ok {
                    s.save_preset();
                }
            });
            if s.user_presets.is_empty() {
                s.hint(ui, "No saved presets yet.");
            }
            for name in s.user_presets.clone() {
                s.preset_row(ui, &name);
            }
        });
    }

    fn preset_row(&mut self, ui: &mut Ui, name: &str) {
        ui.horizontal(|ui| {
            let renaming = self.renaming.as_ref().is_some_and(|r| r.from == name);
            let confirming = self.confirm_delete.as_deref() == Some(name);
            if renaming {
                let mut commit = false;
                let mut cancel = false;
                if let Some(r) = self.renaming.as_mut() {
                    let w = (ui.available_width() - 150.0).clamp(100.0, 420.0);
                    let te = ui.add(egui::TextEdit::singleline(&mut r.buf).desired_width(w));
                    if std::mem::take(&mut r.fresh) {
                        te.request_focus();
                    }
                    commit = te.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    cancel = ui.input(|i| i.key_pressed(egui::Key::Escape));
                    commit |= ui.button("OK").clicked();
                    cancel |= ui.button("Cancel").clicked();
                }
                if commit {
                    if let Some(r) = self.renaming.take() {
                        match presets::rename(&self.preset_dir, &r.from, &r.buf) {
                            Ok(n) => self.notice = Some((format!("Renamed to \"{n}\""), false)),
                            Err(e) => self.error(format!("rename preset: {e}")),
                        }
                        self.refresh_presets();
                    }
                } else if cancel {
                    self.renaming = None;
                }
                return;
            }
            let w = (ui.available_width() - 270.0).max(100.0);
            ui.allocate_ui_with_layout(
                Vec2::new(w, ROW_H),
                Layout::left_to_right(Align::Center),
                |ui| {
                    ui.set_min_size(Vec2::new(w, ROW_H));
                    ui.add(egui::Label::new(RichText::new(name).strong()).truncate())
                        .on_hover_text(name);
                },
            );
            if confirming {
                ui.label(RichText::new("Delete?").color(BAD));
                if ui.button("Yes, delete").clicked() {
                    match presets::delete(&self.preset_dir, name) {
                        Ok(()) => self.notice = Some((format!("Deleted preset \"{name}\""), false)),
                        Err(e) => self.error(format!("delete preset: {e}")),
                    }
                    self.confirm_delete = None;
                    self.refresh_presets();
                }
                if ui.button("Keep").clicked() {
                    self.confirm_delete = None;
                }
            } else {
                if ui
                    .button("Load")
                    .on_hover_text("Apply this preset")
                    .clicked()
                {
                    self.load_preset(name);
                }
                if ui.button("Rename").clicked() {
                    self.confirm_delete = None;
                    self.renaming = Some(Renaming {
                        from: name.to_string(),
                        buf: name.to_string(),
                        fresh: true,
                    });
                }
                if ui.button("Delete").clicked() {
                    self.renaming = None;
                    self.confirm_delete = Some(name.to_string());
                }
            }
        });
    }

    fn uc_import_card(&mut self, ui: &mut Ui) {
        self.card(ui, "Import Universal Control scene", None, |s, ui| {
            ui.horizontal(|ui| {
                Self::label_cell(ui, ".scene file");
                let w = (ui.available_width() - 80.0 - 2.0 * ui.spacing().item_spacing.x)
                    .clamp(100.0, 700.0);
                ui.add(egui::TextEdit::singleline(&mut s.uc_path).desired_width(w));
                if ui
                    .add(egui::Button::new("Import").min_size(Vec2::new(70.0, 26.0)))
                    .clicked()
                {
                    match std::fs::read_to_string(&s.uc_path)
                        .map_err(|e| e.to_string())
                        .and_then(|t| uc_scene_import(&t))
                    {
                        Ok(v) => {
                            let n = s.apply_values(v);
                            s.notice = Some((format!("Imported {n} values from UC scene"), false));
                        }
                        Err(e) => s.error(format!("import: {e}")),
                    }
                }
            });
            ui.label(
                RichText::new("Imports mixer levels, mute/solo, sends and preamp gain.")
                    .small()
                    .color(MUTED_TXT),
            );
        });
    }

    fn connect_screen(&mut self, ui: &mut Ui) {
        ui.vertical_centered(|ui| {
            ui.add_space(ui.available_height() * 0.3);
            let connecting = self.model.state.connection == Connection::Connecting;
            ui.heading(if connecting {
                "Connecting..."
            } else {
                "Plug in your Revelator Dynamic"
            });
            ui.add_space(8.0);
            ui.add(
                egui::Label::new(
                    RichText::new(&self.model.state.connection_detail).color(MUTED_TXT),
                )
                .selectable(true),
            );
            ui.label("The app reconnects automatically when the mic appears.");
            ui.add_space(8.0);
            if ui.button("Retry now").clicked() {
                self.backend.connect();
            }
            if connecting {
                ui.spinner();
            }
        });
    }

    fn tab_bar(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            for (t, n) in [
                (Tab::Mic, "Mic"),
                (Tab::Mixer, "Mixer"),
                (Tab::Fat, "Fat Channel"),
                (Tab::Fx, "Voice FX & Reverb"),
            ] {
                let sel = self.tab == t;
                let b = egui::Button::new(RichText::new(n).size(15.0).strong().color(if sel {
                    Color32::WHITE
                } else {
                    MUTED_TXT
                }))
                .fill(if sel { ACCENT_DK } else { Color32::TRANSPARENT })
                .stroke(Stroke::NONE)
                .min_size(Vec2::new(0.0, 30.0));
                if ui.add(b).clicked() {
                    self.tab = t;
                }
            }
            if ui.available_width() > 230.0 {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.label(RichText::new("unverified guess").small().color(MUTED_TXT));
                    let (r, _) = ui.allocate_exact_size(Vec2::splat(12.0), Sense::hover());
                    ui.painter()
                        .circle_stroke(r.center(), 4.0, Stroke::new(1.5_f32, BAD));
                    ui.label(RichText::new("inferred").small().color(MUTED_TXT));
                    let (r, _) = ui.allocate_exact_size(Vec2::splat(12.0), Sense::hover());
                    ui.painter().circle_filled(r.center(), 4.0, WARN);
                });
            }
        });
    }

    fn status_bar(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            let (txt, col) = match self.model.state.connection {
                Connection::Connected => ("Connected", GOOD),
                Connection::Connecting => ("Connecting", WARN),
                Connection::Disconnected => ("Disconnected", BAD),
            };
            let (r, _) = ui.allocate_exact_size(Vec2::splat(12.0), Sense::hover());
            ui.painter().circle_filled(r.center(), 5.0, col);
            ui.label(RichText::new(txt).color(col))
                .on_hover_text(format!("Backend: {}", self.backend.name()));
            let i = &self.model.state.info;
            if !i.name.is_empty() {
                let t = format!("{}  fw {}  proto {}", i.name, i.firmware, i.protocol);
                ui.add(
                    egui::Label::new(RichText::new(t.clone()).color(MUTED_TXT).small())
                        .selectable(true),
                )
                .on_hover_text(t);
            }
            {
                ui.separator();
                ui.label(RichText::new("Sample rate").small().color(MUTED_TXT))
                    .on_hover_text("The rate Windows uses for the mic (Sound settings > Revelator > Properties > Advanced). The mic does not report it; it is needed for the Echo effect and to design the filters.");
                let sr = self.model.get("sys.samplerate") as u32;
                let known = self.demo || self.model.known("sys.samplerate");
                let from_windows = self.model.on(crate::real::RATE_FROM_WINDOWS_ID);
                if from_windows {
                    ui.label(RichText::new(format!("{sr} Hz")).small())
                        .on_hover_text("Read from Windows (the Revelator's audio endpoint format). Change it in Sound settings.");
                } else {
                egui::ComboBox::from_id_salt("demo_sr")
                    .width(90.0)
                    .selected_text(if known {
                        format!("{sr} Hz")
                    } else {
                        "unknown".into()
                    })
                    .show_ui(ui, |ui| {
                        for r in SAMPLE_RATES {
                            if ui.selectable_label(r == sr, format!("{r} Hz")).clicked() {
                                self.set("sys.samplerate", r as f32);
                            }
                        }
                    });
                }
            }
            ui.separator();
            let mut on = self.autostart;
            if ui
                .checkbox(&mut on, RichText::new("Start on boot").small())
                .on_hover_text("Start Revelator Control hidden in the tray when you sign in to Windows. It is listed in Task Manager > Startup apps, where it can also be turned off.")
                .changed()
            {
                match crate::autostart::set(on) {
                    Ok(()) => self.autostart = crate::autostart::is_enabled(),
                    Err(e) => self.error(e),
                }
            }
            let mut check = self.updater.enabled();
            if ui
                .checkbox(&mut check, RichText::new("Check for updates").small())
                .on_hover_text("Look for a new release on GitHub once a day and show a notification when there is one. Nothing is installed automatically.")
                .changed()
            {
                self.updater.set_enabled(check);
            }
            if let Some(n) = self.model.state.notice.take() {
                self.notice = Some((n, false));
            }
            let err = self.model.state.last_error.clone();
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if let Some(e) = err {
                    if ui.small_button("x").on_hover_text("Dismiss").clicked() {
                        self.model.state.last_error = None;
                    }
                    let t = format!("Error: {e}");
                    ui.add(
                        egui::Label::new(RichText::new(t.clone()).color(BAD))
                            .truncate()
                            .selectable(true),
                    )
                    .on_hover_text(t);
                } else if let Some((n, _)) = &self.notice {
                    ui.add(egui::Label::new(RichText::new(n).color(MUTED_TXT)).truncate());
                }
            });
        });
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.frame(ctx);
    }
}

impl App {
    /// One UI frame (separate from `eframe::App::update` so it can be driven headlessly).
    fn frame(&mut self, ctx: &egui::Context) {
        if self.connect_requested {
            self.connect_requested = false;
        }
        let (minimized, focused) = ctx.input(|i| {
            let v = i.viewport();
            (v.minimized.unwrap_or(false), v.focused.unwrap_or(true))
        });
        // Closing the window keeps the app (and the preset-slot watcher) running in the tray.
        if self.tray.is_some()
            && ctx.input(|i| i.viewport().close_requested())
            && !crate::tray::QUITTING.load(Ordering::SeqCst)
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            crate::tray::hide(self.hwnd);
        }
        if self.start_hidden {
            self.start_hidden = false;
            crate::tray::hide(self.hwnd);
        }
        if focused && !self.was_focused {
            self.autostart = crate::autostart::is_enabled();
        }
        self.was_focused = focused;
        self.visible.store(!minimized, Ordering::Relaxed);
        if !self.wake_set {
            self.wake_set = true;
            let c = ctx.clone();
            let visible = self.visible.clone();
            self.backend.set_wake(Arc::new(move || {
                if visible.load(Ordering::Relaxed) {
                    c.request_repaint();
                }
            }));
        }
        let now = Instant::now();
        if let Some(snap) = self.backend.snapshot() {
            self.model.apply_snapshot(snap, now);
        }
        self.model.expire_pending(now);
        let connected = self.model.state.connection == Connection::Connected;
        if connected {
            if let (Some(a), Some(r)) = (self.startup_action.take(), &self.remote) {
                r(a);
            }
        }
        let queued =
            std::mem::take(&mut *self.remote_queue.lock().unwrap_or_else(|e| e.into_inner()));
        for a in queued {
            match a {
                Action::Slot(n) => self.recall(n),
                Action::ToggleMute => {
                    let muted = self.model.on("mic.mute");
                    self.set("mic.mute", if muted { 0.0 } else { 1.0 });
                }
            }
        }
        // Also from the UI: covers backends without a status hook (demo) and slot clicks
        // shown optimistically before the device confirms them.
        if let Some(t) = &self.taskbar {
            t.set(crate::taskbar::status_of(&self.model.state));
        }
        // Repaint on demand: the device thread wakes us when state changes. Nothing is
        // scheduled while minimized. Only a demo backend (moving meters) needs a steady
        // frame rate, and a slower one when the window is not focused.
        if minimized {
            // Restoring the window repaints it; pending values expire on that frame.
        } else if self.backend.animated() {
            // Meters only exist on some tabs.
            if matches!(self.tab, Tab::Mic | Tab::Mixer | Tab::Fat) {
                self.backend.poll_meters(&mut self.model.state.meters);
                ctx.request_repaint_after(if focused {
                    METER_PERIOD_FOCUSED
                } else {
                    METER_PERIOD_UNFOCUSED
                });
            }
        } else if self.model.has_pending() {
            ctx.request_repaint_after(self.model.hold + Duration::from_millis(20));
        }
        if self.update_peaks(now) && !minimized {
            ctx.request_repaint_after(METER_PERIOD_FOCUSED);
        }

        egui::TopBottomPanel::bottom("status")
            .frame(
                egui::Frame::none()
                    .fill(Color32::from_rgb(0x0e, 0x10, 0x14))
                    .inner_margin(egui::Margin::symmetric(14.0, 6.0)),
            )
            .show(ctx, |ui| self.status_bar(ui));
        if let Some(rel) = self.updater.available() {
            egui::TopBottomPanel::top("update")
                .frame(
                    egui::Frame::none()
                        .fill(Color32::from_rgb(0x1a, 0x2a, 0x3d))
                        .inner_margin(egui::Margin::symmetric(14.0, 6.0)),
                )
                .show(ctx, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(format!(
                            "Revelator Control {} is available (you have {}).",
                            rel.version,
                            env!("CARGO_PKG_VERSION")
                        ));
                        ui.hyperlink_to("Download", &rel.url);
                        if ui.small_button("Skip this version").clicked() {
                            self.updater.skip();
                        }
                    });
                });
        }
        let connected = self.model.state.connection == Connection::Connected;
        if connected {
            egui::TopBottomPanel::top("tabs")
                .frame(
                    egui::Frame::none()
                        .fill(Color32::from_rgb(0x0e, 0x10, 0x14))
                        .inner_margin(egui::Margin::symmetric(14.0, 6.0)),
                )
                .show(ctx, |ui| self.tab_bar(ui));
        }
        egui::CentralPanel::default()
            .frame(egui::Frame::none().fill(BG).inner_margin(14.0))
            .show(ctx, |ui| {
                if !connected {
                    self.connect_screen(ui);
                    return;
                }
                let tab = self.tab;
                egui::ScrollArea::vertical()
                    .id_salt(("tab", tab as u8))
                    .auto_shrink([false; 2])
                    .show(ui, |ui| {
                        let avail = ui.available_width();
                        let w = avail.min(MAX_CONTENT_W);
                        ui.horizontal_top(|ui| {
                            ui.add_space((avail - w) / 2.0);
                            ui.vertical(|ui| {
                                ui.set_width(w - ui.spacing().item_spacing.x);
                                match tab {
                                    Tab::Mic => self.mic_tab(ui),
                                    Tab::Mixer => self.mixer_tab(ui),
                                    Tab::Fat => self.fat_tab(ui),
                                    Tab::Fx => self.fx_tab(ui),
                                }
                            });
                        });
                    });
            });
    }
}

#[cfg(test)]
mod bench {
    use super::*;
    use crate::demo::DemoBackend;
    use std::time::Instant;

    /// Headless frame-time measurement while the pointer sweeps across the window.
    /// Run: `cargo test -p revelator-gui --release bench_frames -- --nocapture --ignored`
    #[test]
    #[ignore]
    fn bench_frames() {
        let ctx = egui::Context::default();
        setup_style(&ctx);
        let mut app = App::new(Box::new(DemoBackend::new()), true);
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, Vec2::new(1100.0, 760.0));
        let mut t = 0.0f64;
        let mut run = |app: &mut App, pos: Option<egui::Pos2>| -> (f64, f64) {
            t += 0.016;
            let mut input = egui::RawInput {
                screen_rect: Some(screen),
                time: Some(t),
                ..Default::default()
            };
            input.events.push(match pos {
                Some(p) => egui::Event::PointerMoved(p),
                None => egui::Event::PointerGone,
            });
            let a = Instant::now();
            let out = ctx.run(input, |c| app.frame(c));
            let b = Instant::now();
            let _ = ctx.tessellate(out.shapes, out.pixels_per_point);
            let c = Instant::now();
            ((b - a).as_secs_f64() * 1e3, (c - b).as_secs_f64() * 1e3)
        };
        for _ in 0..5 {
            run(&mut app, None); // warm up fonts / layout caches
        }
        for tab in ["mic", "mixer", "fat", "fx"] {
            app.select_tab(tab);
            for _ in 0..5 {
                run(&mut app, None);
            }
            let n = 300;
            let (mut ui_ms, mut tess_ms) = (0.0, 0.0);
            for i in 0..n {
                let x = 20.0 + (i as f32 * 37.0) % 1050.0;
                let y = 60.0 + (i as f32 * 23.0) % 680.0;
                let (a, b) = run(&mut app, Some(egui::pos2(x, y)));
                ui_ms += a;
                tess_ms += b;
            }
            println!(
                "{tab:8} ui {:.2} ms  tessellate {:.2} ms  per frame",
                ui_ms / n as f64,
                tess_ms / n as f64
            );
        }
    }
}

#[cfg(test)]
mod peak_tests {
    use super::*;

    #[test]
    fn peak_holds_then_falls() {
        let mut m = HashMap::new();
        let t0 = Instant::now();
        let at = |ms| t0 + Duration::from_millis(ms);
        assert_eq!(track_peak(&mut m, "meter.mic", -6.0, t0), -6.0);
        // held while the level drops
        assert_eq!(track_peak(&mut m, "meter.mic", -30.0, at(1000)), -6.0);
        // 1 s after the hold ends: 12 dB lower
        let v = track_peak(&mut m, "meter.mic", -30.0, at(2500));
        assert!((v + 18.0).abs() < 0.01, "{v}");
        // never below the live level; a new maximum restarts the hold
        assert_eq!(track_peak(&mut m, "meter.mic", -30.0, at(10_000)), -30.0);
        assert_eq!(track_peak(&mut m, "meter.mic", -3.0, at(11_000)), -3.0);
        // gain reduction falls to 0, not to the dB floor
        track_peak(&mut m, "meter.reduction", 6.0, t0);
        assert_eq!(track_peak(&mut m, "meter.reduction", 0.0, at(60_000)), 0.0);
    }
}
