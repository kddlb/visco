//! Waveform viewer: stacked lanes over a shared, zoomable time axis, with block strips.

use std::{cell::RefCell, collections::HashMap, rc::Rc, sync::Arc};

use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use visco_core::{BlockId, BlockStrip, Lane, Plane, Ramp, SeriesRole, SignalView};

use crate::{raster, viewer::SelectFn};

const STRIP_H: f32 = 9.0;
const MIN_SPAN: f64 = 16.0;

/// Category colors for block strips (index = legend position).
pub const PALETTE: [u32; 8] = [0x8a8f98, 0xe5484d, 0xf5a524, 0x3b82f6, 0x10b981, 0xa855f7, 0xec4899, 0x14b8a6];

pub fn role_color(role: SeriesRole) -> Hsla {
    match role {
        SeriesRole::Primary => rgb(0x4ea1ff).into(),
        SeriesRole::Secondary => hsla(0.11, 1.0, 0.55, 0.85),
        SeriesRole::Residual => rgb(0x5ad18a).into(),
        SeriesRole::Error => rgb(0xff5a5a).into(),
    }
}

/// Min/max summaries at power-of-4 block sizes for fast overview drawing.
struct Pyramid {
    /// levels[i] holds (min, max) per block of 4^(i+2) samples.
    levels: Vec<Vec<(f32, f32)>>,
}

impl Pyramid {
    fn build(x: &[f32]) -> Self {
        let mut levels = vec![];
        let mut cur: Vec<(f32, f32)> =
            x.chunks(16).map(|c| c.iter().fold((f32::MAX, f32::MIN), |(a, b), &v| (a.min(v), b.max(v)))).collect();
        while cur.len() > 1 {
            let next = cur.chunks(4).map(|c| c.iter().fold((f32::MAX, f32::MIN), |(a, b), &(lo, hi)| (a.min(lo), b.max(hi)))).collect();
            levels.push(cur);
            cur = next;
        }
        levels.push(cur);
        Self { levels }
    }

    /// Min/max of `x[start..end]` using the coarsest level whose blocks fit `res` samples.
    fn range(&self, x: &[f32], start: usize, end: usize, res: f64) -> (f32, f32) {
        let end = end.min(x.len());
        if start >= end {
            return (0.0, 0.0);
        }
        let mut lvl = None;
        let mut bsz = 16usize;
        for i in 0..self.levels.len() {
            if bsz as f64 <= res {
                lvl = Some((i, bsz));
            }
            bsz *= 4;
        }
        let Some((i, b)) = lvl else {
            return x[start..end].iter().fold((f32::MAX, f32::MIN), |(a, c), &v| (a.min(v), c.max(v)));
        };
        let (b0, b1) = (start / b, end.div_ceil(b).min(self.levels[i].len()));
        self.levels[i][b0..b1].iter().fold((f32::MAX, f32::MIN), |(a, c), &(lo, hi)| (a.min(lo), c.max(hi)))
    }
}

struct Drag {
    start: Point<Pixels>,
    view0: (f64, f64),
    moved: bool,
}

#[derive(Default)]
pub struct SignalState {
    /// Visible (first sample, sample count); `None` shows everything.
    pub view: Option<(f64, f64)>,
    /// Vertical gain.
    pub gain: f32,
    drag: Option<Drag>,
    width: f32,
    left: f32,
    len: f64,
    pyramids: HashMap<usize, Rc<Pyramid>>,
    spectro: Option<(usize, Arc<RenderImage>)>,
    /// (lane, sample) under the pointer.
    pub hover: Option<(usize, u64)>,
    /// Playback position in samples.
    pub playhead: Option<f64>,
}

impl SignalState {
    pub fn reset(&mut self) {
        self.view = None;
        self.gain = 1.0;
        self.drag = None;
    }

    pub fn visible(&self, len: u64) -> (f64, f64) {
        let len = len.max(1) as f64;
        match self.view {
            Some((s, n)) => (s, n),
            None => (0.0, len),
        }
    }

    fn set_view(&mut self, start: f64, n: f64) {
        let n = n.clamp(MIN_SPAN, self.len.max(MIN_SPAN));
        let start = start.clamp(0.0, (self.len - n).max(0.0));
        self.view = if start <= 0.0 && n >= self.len { None } else { Some((start, n)) };
    }

    pub fn zoom_by(&mut self, factor: f64, len: u64) {
        self.len = len as f64;
        let (s, n) = self.visible(len);
        let c = s + n / 2.0;
        let n2 = n / factor;
        self.set_view(c - n2 / 2.0, n2);
    }

    /// Show a sample range with a little context.
    pub fn focus(&mut self, start: u64, end: u64, len: u64) {
        self.len = len as f64;
        let w = (end - start).max(1) as f64;
        let (_, n) = self.visible(len);
        let n = if n > w * 12.0 || n < w * 1.2 { w * 3.0 } else { n };
        self.set_view(start as f64 + w / 2.0 - n / 2.0, n);
    }

    fn pyramid(&mut self, data: &Arc<[f32]>) -> Rc<Pyramid> {
        let key = data.as_ptr() as usize;
        if self.pyramids.len() > 48 {
            self.pyramids.clear();
        }
        self.pyramids.entry(key).or_insert_with(|| Rc::new(Pyramid::build(data))).clone()
    }
}

pub struct SignalProps {
    pub view: SignalView,
    pub selected: Option<BlockId>,
    pub highlight: Option<BlockId>,
    pub state: Rc<RefCell<SignalState>>,
    pub on_select: SelectFn,
    pub on_hover: crate::viewer::HoverFn,
    pub legend_text: Hsla,
}

fn x_of(bounds: &Bounds<Pixels>, view: (f64, f64), sample: f64) -> Pixels {
    bounds.left() + px(((sample - view.0) / view.1 * f32::from(bounds.size.width) as f64) as f32)
}

fn sample_of(bounds: &Bounds<Pixels>, view: (f64, f64), x: Pixels) -> f64 {
    view.0 + (f32::from(x - bounds.left()) as f64 / f32::from(bounds.size.width).max(1.0) as f64) * view.1
}

fn strip_color(strip: &BlockStrip, i: usize, heat_max: f32) -> Option<Hsla> {
    if let Some(c) = &strip.categories {
        return Some(rgb(PALETTE[c[i] as usize % PALETTE.len()]).into());
    }
    if let Some(h) = &strip.heat {
        let [r, g, b] = raster::ramp_rgb(Ramp::Heat, h[i], (0.0, heat_max));
        return Some(rgb(((r as u32) << 16) | ((g as u32) << 8) | b as u32).into());
    }
    None
}

fn lane_canvas(
    lane_ix: usize,
    lane: Lane,
    len: u64,
    props: Rc<(Option<BlockId>, Option<BlockId>, SelectFn, crate::viewer::HoverFn)>,
    state: Rc<RefCell<SignalState>>,
) -> impl IntoElement {
    canvas(
        |bounds, window, _| window.insert_hitbox(bounds, HitboxBehavior::Normal),
        move |bounds, hitbox, window, _| {
            let (selected, highlight, on_select, on_hover) = (&props.0, &props.1, props.2.clone(), props.3.clone());
            {
                let mut s = state.borrow_mut();
                s.width = f32::from(bounds.size.width);
                s.left = f32::from(bounds.left());
                s.len = len as f64;
            }
            let view = state.borrow().visible(len);
            let gain = state.borrow().gain.max(0.05);
            let scale = window.scale_factor();
            let has_strip = lane.blocks.is_some();
            let wave = Bounds {
                origin: bounds.origin,
                size: size(bounds.size.width, bounds.size.height - if has_strip { px(STRIP_H + 2.0) } else { px(0.0) }),
            };
            window.with_content_mask(Some(ContentMask { bounds }), |window| {
                window.paint_quad(fill(bounds, rgb(0x16161a)));
                let mid = wave.center().y;
                let half = f32::from(wave.size.height) / 2.0 * 0.92;
                let y_of = |v: f32| mid - px((v * gain).clamp(-1.1, 1.1) * half);
                window.paint_quad(fill(Bounds { origin: point(wave.left(), mid), size: size(wave.size.width, px(1.0 / scale)) }, hsla(0.0, 0.0, 1.0, 0.12)));

                // Block shading (selected / highlighted) and boundaries.
                if let Some(strip) = &lane.blocks {
                    let first = strip.ranges.partition_point(|(r, _)| (r.end as f64) < view.0);
                    let px_per_sample = f32::from(bounds.size.width) as f64 / view.1;
                    let heat_max = strip.heat.as_ref().map_or(1.0, |h| h.iter().cloned().fold(1e-6, f32::max));
                    let strip_top = bounds.bottom() - px(STRIP_H);
                    for i in first..strip.ranges.len() {
                        let (r, id) = &strip.ranges[i];
                        if r.start as f64 > view.0 + view.1 {
                            break;
                        }
                        let (x0, x1) = (x_of(&bounds, view, r.start as f64), x_of(&bounds, view, r.end as f64));
                        let w = (x1 - x0).max(px(1.0 / scale));
                        let is_sel = Some(*id) == *selected;
                        let is_hi = Some(*id) == *highlight;
                        if is_sel || is_hi {
                            let c = if is_sel { hsla(0.52, 0.9, 0.55, 0.16) } else { hsla(0.14, 1.0, 0.55, 0.16) };
                            window.paint_quad(fill(Bounds { origin: point(x0, wave.top()), size: size(w, wave.size.height) }, c));
                        }
                        if let Some(c) = strip_color(strip, i, heat_max) {
                            window.paint_quad(fill(Bounds { origin: point(x0, strip_top), size: size(w, px(STRIP_H)) }, c));
                        } else {
                            window.paint_quad(fill(Bounds { origin: point(x0, strip_top), size: size(w, px(STRIP_H)) }, hsla(0.0, 0.0, 1.0, if i % 2 == 0 { 0.10 } else { 0.18 })));
                        }
                        if (r.end - r.start) as f64 * px_per_sample > 6.0 {
                            let c = hsla(0.0, 0.0, 1.0, 0.13);
                            window.paint_quad(fill(Bounds { origin: point(x0, wave.top()), size: size(px(1.0 / scale), bounds.size.height) }, c));
                        }
                        if is_sel {
                            let b = Bounds { origin: point(x0, wave.top()), size: size(w, bounds.size.height) };
                            window.paint_quad(quad(b, px(0.0), transparent_black(), px(1.5), hsla(0.52, 1.0, 0.6, 1.0), BorderStyle::Solid));
                        }
                    }
                }

                // Waveforms.
                let width = f32::from(wave.size.width).max(1.0);
                let spp = view.1 / width as f64; // samples per logical pixel
                for series in &lane.series {
                    let color = role_color(series.role);
                    let data = &series.data;
                    if spp > 1.0 {
                        let pyr = state.borrow_mut().pyramid(data);
                        let cols = (width * scale) as usize;
                        let col_w = px(1.0 / scale);
                        for c in 0..cols {
                            let s0 = view.0 + c as f64 * view.1 / cols as f64;
                            let s1 = s0 + view.1 / cols as f64;
                            let (lo, hi) = pyr.range(data, s0.max(0.0) as usize, s1.ceil() as usize, view.1 / cols as f64);
                            if lo > hi {
                                continue;
                            }
                            let (top, bot) = (y_of(hi), y_of(lo));
                            let x = wave.left() + px(c as f32 / scale);
                            window.paint_quad(fill(Bounds { origin: point(x, top), size: size(col_w, (bot - top).max(col_w)) }, color));
                        }
                    } else {
                        let s0 = view.0.floor().max(0.0) as usize;
                        let s1 = ((view.0 + view.1).ceil() as usize + 1).min(data.len());
                        for chunk_start in (s0..s1).step_by(4000) {
                            let end = (chunk_start + 4001).min(s1);
                            let mut path = PathBuilder::stroke(px(1.2));
                            for (k, i) in (chunk_start..end).enumerate() {
                                let p = point(x_of(&wave, view, i as f64 + 0.5), y_of(data[i]));
                                if k == 0 { path.move_to(p) } else { path.line_to(p) }
                            }
                            if let Ok(p) = path.build() {
                                window.paint_path(p, color);
                            }
                        }
                        if spp < 0.12 {
                            for i in s0..s1 {
                                let p = point(x_of(&wave, view, i as f64 + 0.5), y_of(data[i]));
                                window.paint_quad(fill(Bounds { origin: point(p.x - px(1.5), p.y - px(1.5)), size: size(px(3.0), px(3.0)) }, color));
                            }
                        }
                    }
                }

                if let Some(ph) = state.borrow().playhead {
                    let x = x_of(&bounds, view, ph);
                    window.paint_quad(fill(Bounds { origin: point(x, bounds.top()), size: size(px(1.5), bounds.size.height) }, rgb(0xffffff)));
                }
            });

            // --- events
            let st = state.clone();
            let hb = hitbox.clone();
            window.on_mouse_event(move |ev: &MouseDownEvent, phase, window, _| {
                if phase == DispatchPhase::Bubble && ev.button == MouseButton::Left && hb.is_hovered(window) {
                    let mut s = st.borrow_mut();
                    let v = s.visible(len);
                    s.drag = Some(Drag { start: ev.position, view0: v, moved: false });
                    window.capture_pointer(hb.id);
                }
            });

            let st = state.clone();
            let hb = hitbox.clone();
            let hover = on_hover.clone();
            window.on_mouse_event(move |ev: &MouseMoveEvent, phase, window, cx| {
                if phase != DispatchPhase::Bubble {
                    return;
                }
                let mut s = st.borrow_mut();
                if let Some(d) = s.drag.as_mut() {
                    let dx = f32::from(ev.position.x - d.start.x);
                    if !d.moved && dx.abs() > 3.0 {
                        d.moved = true;
                    }
                    if d.moved && ev.pressed_button == Some(MouseButton::Left) {
                        let (s0, n) = d.view0;
                        let w = s.width.max(1.0) as f64;
                        s.set_view(s0 - dx as f64 / w * n, n);
                        window.refresh();
                    }
                }
                let h = hb.is_hovered(window).then(|| (lane_ix, sample_of(&bounds, s.visible(len), ev.position.x).max(0.0) as u64));
                if h != s.hover && (h.is_some() || s.hover.is_some_and(|(l, _)| l == lane_ix)) {
                    s.hover = h;
                    drop(s);
                    hover(window, cx);
                }
            });

            let st = state.clone();
            let blocks = lane.blocks.clone();
            let select = on_select.clone();
            let hb = hitbox.clone();
            window.on_mouse_event(move |ev: &MouseUpEvent, phase, window, cx| {
                if phase != DispatchPhase::Bubble || ev.button != MouseButton::Left {
                    return;
                }
                let mut s = st.borrow_mut();
                let Some(d) = s.drag.take() else { return };
                window.release_pointer();
                if d.moved || !bounds.contains(&ev.position) {
                    return;
                }
                let _ = &hb;
                let sample = sample_of(&bounds, s.visible(len), ev.position.x).max(0.0) as u64;
                drop(s);
                if let Some(b) = &blocks {
                    select(b.at(sample).map(|i| b.ranges[i].1), window, cx);
                }
            });

            let st = state.clone();
            let hb = hitbox.clone();
            window.on_mouse_event(move |ev: &ScrollEvent, phase, window, _| {
                if phase != DispatchPhase::Bubble || !hb.is_hovered(window) {
                    return;
                }
                let mut s = st.borrow_mut();
                let d = ev.delta.pixel_delta(px(16.0));
                let (dx, dy) = (f32::from(d.x) as f64, f32::from(d.y) as f64);
                let (s0, n) = s.visible(len);
                if ev.modifiers.alt {
                    s.gain = (s.gain * (dy as f32 * 0.01).exp()).clamp(0.25, 64.0);
                } else if ev.modifiers.platform || ev.modifiers.control {
                    let at = sample_of(&bounds, (s0, n), ev.position.x);
                    let n2 = n * (-dy * 0.01).exp().recip();
                    let frac = (at - s0) / n;
                    s.set_view(at - frac * n2, n2);
                } else {
                    let w = s.width.max(1.0) as f64;
                    let shift = if dx.abs() > dy.abs() { -dx } else { -dy };
                    s.set_view(s0 + shift / w * n, n);
                }
                window.refresh();
            });

            let st = state.clone();
            let hb = hitbox;
            window.on_mouse_event(move |ev: &PinchEvent, phase, window, _| {
                if phase != DispatchPhase::Bubble || !hb.is_hovered(window) {
                    return;
                }
                let mut s = st.borrow_mut();
                let (s0, n) = s.visible(len);
                let at = sample_of(&bounds, (s0, n), ev.position.x);
                let n2 = n / (1.0 + ev.delta as f64);
                let frac = (at - s0) / n;
                s.set_view(at - frac * n2, n2);
                window.refresh();
            });
        },
    )
    .size_full()
}

fn spectrogram_canvas(plane: Plane, len: u64, state: Rc<RefCell<SignalState>>) -> impl IntoElement {
    canvas(
        |_, _, _| {},
        move |bounds, _, window, _| {
            let view = state.borrow().visible(len);
            let key = match &plane.kind {
                visco_core::PlaneKind::Scalar { data, .. } => data.as_ptr() as usize,
                _ => 0,
            };
            let img = {
                let mut s = state.borrow_mut();
                match &s.spectro {
                    Some((k, img)) if *k == key => img.clone(),
                    _ => {
                        let img = raster::render_image(&plane, 1);
                        if let Some((_, old)) = s.spectro.replace((key, img.clone())) {
                            let _ = window.drop_image(old);
                        }
                        img
                    }
                }
            };
            let (x0, x1) = (x_of(&bounds, view, 0.0), x_of(&bounds, view, len as f64));
            let ib = Bounds { origin: point(x0, bounds.top()), size: size(x1 - x0, bounds.size.height) };
            window.paint_quad(fill(bounds, rgb(0x000000)));
            let _ = window.paint_image(bounds, ib, Corners::default(), img, 0, false);
            if let Some(ph) = state.borrow().playhead {
                let x = x_of(&bounds, view, ph);
                window.paint_quad(fill(Bounds { origin: point(x, bounds.top()), size: size(px(1.5), bounds.size.height) }, rgb(0xffffff)));
            }
        },
    )
    .size_full()
}

pub fn signal_viewer(p: SignalProps) -> impl IntoElement {
    let SignalProps { view, selected, highlight, state, on_select, on_hover, legend_text } = p;
    let len = view.len;
    state.borrow_mut().len = len as f64;
    if state.borrow().gain == 0.0 {
        state.borrow_mut().gain = 1.0;
    }
    let shared = Rc::new((selected, highlight, on_select, on_hover));
    let mut col = v_flex().size_full().gap_1().px_4();
    if let Some(spec) = view.spectrogram.clone() {
        col = col.child(div().h(px(90.)).flex_none().child(spectrogram_canvas(spec, len, state.clone())));
    }
    for (i, lane) in view.lanes.into_iter().enumerate() {
        let mut header = h_flex().gap_3().text_xs().child(div().font_weight(FontWeight::SEMIBOLD).child(lane.label.clone()));
        for s in &lane.series {
            header = header.child(
                h_flex()
                    .gap_1()
                    .child(div().w(px(10.)).h(px(3.)).bg(role_color(s.role)))
                    .child(div().text_color(legend_text).child(s.label.clone())),
            );
        }
        if let Some(b) = &lane.blocks {
            if b.categories.is_some() {
                for (k, name) in b.legend.iter().enumerate() {
                    header = header.child(
                        h_flex()
                            .gap_1()
                            .child(div().w(px(8.)).h(px(8.)).bg(rgb(PALETTE[k % PALETTE.len()])))
                            .child(div().text_color(legend_text).child(name.clone())),
                    );
                }
            } else if b.heat.is_some() {
                header = header.child(div().text_color(legend_text).child("strip: bits per sample (brighter = more)"));
            }
        }
        col = col.child(
            v_flex()
                .flex_1()
                .min_h(px(60.))
                .child(header)
                .child(div().flex_1().min_h_0().when(true, |d| d.child(lane_canvas(i, lane, len, shared.clone(), state.clone())))),
        );
    }
    col
}
