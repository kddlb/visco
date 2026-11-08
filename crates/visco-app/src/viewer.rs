//! Zoomable, pannable plane viewer with a clickable block overlay, drawn on a GPUI canvas.

use std::{cell::RefCell, rc::Rc, sync::Arc};

use gpui_kit::*;
use visco_core::{BlockGrid, BlockId, Plane, PlaneKind};

use crate::raster;

const MIN_ZOOM: f32 = 0.02;
const MAX_ZOOM: f32 = 96.0;
/// Above this device zoom, pixels are drawn as quads for perfectly crisp edges.
const QUAD_ZOOM: f32 = 6.0;
const MAX_QUADS: usize = 60_000;

struct Drag {
    start: Point<Pixels>,
    center0: (f32, f32),
    moved: bool,
}

type CacheKey = (usize, usize, usize, usize);
pub type SelectFn = Rc<dyn Fn(Option<BlockId>, &mut Window, &mut App)>;
pub type HoverFn = Rc<dyn Fn(&mut Window, &mut App)>;

#[derive(Default)]
pub struct ViewerState {
    /// Logical pixels per source pixel; `None` fits the plane to the viewport.
    pub zoom: Option<f32>,
    /// Source coordinate shown at the viewport center (used when `zoom` is set).
    pub center: (f32, f32),
    drag: Option<Drag>,
    last_bounds: Option<Bounds<Pixels>>,
    last_fit: f32,
    cache: Option<(CacheKey, Arc<RenderImage>)>,
    /// Source pixel under the pointer.
    pub hover: Option<(usize, usize)>,
}

impl ViewerState {
    pub fn reset(&mut self) {
        self.zoom = None;
        self.drag = None;
    }

    /// Current effective zoom (fit zoom when fitting).
    pub fn effective_zoom(&self) -> f32 {
        self.zoom.unwrap_or(self.last_fit)
    }

    pub fn zoom_by(&mut self, factor: f32, plane_size: (usize, usize)) {
        if self.zoom.is_none() {
            self.center = (plane_size.0 as f32 / 2.0, plane_size.1 as f32 / 2.0);
        }
        let z = (self.effective_zoom() * factor).clamp(MIN_ZOOM, MAX_ZOOM);
        self.zoom = Some(z);
    }

    pub fn set_zoom(&mut self, z: f32, plane_size: (usize, usize)) {
        if self.zoom.is_none() {
            self.center = (plane_size.0 as f32 / 2.0, plane_size.1 as f32 / 2.0);
        }
        self.zoom = Some(z.clamp(MIN_ZOOM, MAX_ZOOM));
    }

    /// Center the view on a source rectangle, zooming in if it is tiny.
    pub fn focus(&mut self, x: f32, y: f32, w: f32, h: f32, plane_size: (usize, usize)) {
        let z = self.effective_zoom();
        if z * w.max(h) < 48.0 {
            self.set_zoom(48.0 / w.max(h), plane_size);
        } else if self.zoom.is_none() {
            self.zoom = Some(z);
        }
        self.center = (x + w / 2.0, y + h / 2.0);
    }
}

fn plane_key(p: &Plane, k: usize) -> CacheKey {
    let ptr = match &p.kind {
        PlaneKind::Gray(d) | PlaneKind::Rgb(d) => d.as_ptr() as usize,
        PlaneKind::Scalar { data, .. } => data.as_ptr() as usize,
    };
    (ptr, p.width, p.height, k)
}

/// Position of a block within the grid.
pub fn block_cell(grid: &BlockGrid, id: BlockId) -> Option<(usize, usize)> {
    grid.cells.iter().position(|&c| c == id).map(|i| (i % grid.cols, i / grid.cols))
}

pub struct ViewerProps {
    pub plane: Plane,
    pub grid: Option<BlockGrid>,
    pub selected: Option<BlockId>,
    /// Secondary highlight (e.g. the block owning the hovered bit span).
    pub highlight: Option<BlockId>,
    pub state: Rc<RefCell<ViewerState>>,
    pub on_select: SelectFn,
    /// Called when the hovered pixel changes, so the owner can re-render its status line.
    pub on_hover: HoverFn,
}

struct Geometry {
    scale: f32,
    origin: Point<Pixels>,
}

impl Geometry {
    fn new(state: &mut ViewerState, bounds: Bounds<Pixels>, pw: usize, ph: usize) -> Self {
        let fit = ((f32::from(bounds.size.width) / pw as f32).min(f32::from(bounds.size.height) / ph as f32) * 0.96)
            .clamp(MIN_ZOOM, MAX_ZOOM);
        state.last_fit = fit;
        state.last_bounds = Some(bounds);
        let scale = state.zoom.unwrap_or(fit);
        let center = if state.zoom.is_some() { state.center } else { (pw as f32 / 2.0, ph as f32 / 2.0) };
        let c = bounds.center();
        let origin = point(c.x - px(center.0 * scale), c.y - px(center.1 * scale));
        Self { scale, origin }
    }

    fn to_screen(&self, x: f32, y: f32) -> Point<Pixels> {
        point(self.origin.x + px(x * self.scale), self.origin.y + px(y * self.scale))
    }

    fn to_source(&self, p: Point<Pixels>) -> (f32, f32) {
        (f32::from(p.x - self.origin.x) / self.scale, f32::from(p.y - self.origin.y) / self.scale)
    }

    fn rect(&self, x: f32, y: f32, w: f32, h: f32) -> Bounds<Pixels> {
        Bounds { origin: self.to_screen(x, y), size: size(px(w * self.scale), px(h * self.scale)) }
    }
}

fn source_pixel(g: &Geometry, p: Point<Pixels>, pw: usize, ph: usize) -> Option<(usize, usize)> {
    let (x, y) = g.to_source(p);
    (x >= 0.0 && y >= 0.0 && (x as usize) < pw && (y as usize) < ph).then_some((x as usize, y as usize))
}

fn block_at(grid: &BlockGrid, x: usize, y: usize) -> Option<BlockId> {
    let (c, r) = (x / grid.block_w, y / grid.block_h);
    (c < grid.cols && r < grid.rows).then(|| grid.cells[r * grid.cols + c])
}

pub fn viewer(props: ViewerProps) -> impl IntoElement {
    let ViewerProps { plane, grid, selected, highlight, state, on_select, on_hover } = props;
    let (pw, ph) = (plane.width, plane.height);
    canvas(
        |bounds, window, _| window.insert_hitbox(bounds, HitboxBehavior::Normal),
        move |bounds, hitbox, window, _cx| {
            if pw == 0 || ph == 0 {
                return;
            }
            let g = Geometry::new(&mut state.borrow_mut(), bounds, pw, ph);
            let scale_factor = window.scale_factor();
            let device_zoom = g.scale * scale_factor;

            window.with_content_mask(Some(ContentMask { bounds }), |window| {
                window.paint_quad(fill(bounds, rgb(0x1b1b1f)));
                let img_bounds = g.rect(0.0, 0.0, pw as f32, ph as f32);

                // Visible source rectangle.
                let (x0, y0) = g.to_source(bounds.origin);
                let (x1, y1) = g.to_source(bounds.bottom_right());
                let vx0 = (x0.floor().max(0.0)) as usize;
                let vy0 = (y0.floor().max(0.0)) as usize;
                let vx1 = (x1.ceil().max(0.0) as usize).min(pw);
                let vy1 = (y1.ceil().max(0.0) as usize).min(ph);
                let visible = vx1.saturating_sub(vx0) * vy1.saturating_sub(vy0);

                if device_zoom >= QUAD_ZOOM && visible <= MAX_QUADS {
                    for y in vy0..vy1 {
                        for x in vx0..vx1 {
                            window.paint_quad(fill(g.rect(x as f32, y as f32, 1.0, 1.0), raster::pixel_hsla(&plane, x, y)));
                        }
                    }
                } else {
                    let k = raster::prescale_factor(&plane, device_zoom.ceil() as usize);
                    let key = plane_key(&plane, k);
                    let image = {
                        let mut st = state.borrow_mut();
                        match &st.cache {
                            Some((ck, img)) if *ck == key => img.clone(),
                            _ => {
                                let img = raster::render_image(&plane, k);
                                if let Some((_, old)) = st.cache.replace((key, img.clone())) {
                                    let _ = window.drop_image(old);
                                }
                                img
                            }
                        }
                    };
                    let _ = window.paint_image(bounds, img_bounds, Corners::default(), image, 0, false);
                }

                if let Some(grid) = &grid {
                    let cell_px = g.scale * grid.block_w.min(grid.block_h) as f32;
                    if cell_px >= 6.0 {
                        let alpha = ((cell_px - 6.0) / 30.0).clamp(0.12, 0.35);
                        let line = hsla(0.0, 0.0, 1.0, alpha);
                        let thin = px(1.0 / scale_factor);
                        let (c0, c1) = (vx0 / grid.block_w, (vx1.div_ceil(grid.block_w)).min(grid.cols));
                        let (r0, r1) = (vy0 / grid.block_h, (vy1.div_ceil(grid.block_h)).min(grid.rows));
                        let top = g.to_screen(0.0, (r0 * grid.block_h) as f32).y.max(img_bounds.top());
                        let bottom = g.to_screen(0.0, (r1 * grid.block_h).min(ph) as f32).y.min(img_bounds.bottom());
                        let left = g.to_screen((c0 * grid.block_w) as f32, 0.0).x.max(img_bounds.left());
                        let right = g.to_screen((c1 * grid.block_w).min(pw) as f32, 0.0).x.min(img_bounds.right());
                        for c in c0..=c1 {
                            let x = g.to_screen((c * grid.block_w).min(pw) as f32, 0.0).x;
                            window.paint_quad(fill(Bounds { origin: point(x, top), size: size(thin, bottom - top) }, line));
                        }
                        for r in r0..=r1 {
                            let y = g.to_screen(0.0, (r * grid.block_h).min(ph) as f32).y;
                            window.paint_quad(fill(Bounds { origin: point(left, y), size: size(right - left, thin) }, line));
                        }
                    }
                    let mut mark = |id: BlockId, color: Hsla, width: f32| {
                        if let Some((c, r)) = block_cell(grid, id) {
                            let b = g.rect(
                                (c * grid.block_w) as f32,
                                (r * grid.block_h) as f32,
                                grid.block_w as f32,
                                grid.block_h as f32,
                            );
                            let b = b.dilate(px(width));
                            window.paint_quad(quad(b, px(1.0), transparent_black(), px(width), color, BorderStyle::Solid));
                        }
                    };
                    if let Some(h) = highlight.filter(|h| Some(*h) != selected) {
                        mark(h, hsla(0.14, 1.0, 0.6, 0.9), 1.5);
                    }
                    if let Some(s) = selected {
                        mark(s, hsla(0.52, 1.0, 0.6, 1.0), 2.0);
                    }
                }
            });

            if hitbox.is_hovered(window) {
                window.set_cursor_style(
                    if state.borrow().drag.as_ref().is_some_and(|d| d.moved) { CursorStyle::ClosedHand } else { CursorStyle::Crosshair },
                    &hitbox,
                );
            }

            // --- events
            let st = state.clone();
            let hb = hitbox.clone();
            window.on_mouse_event(move |ev: &MouseDownEvent, phase, window, _| {
                if phase == DispatchPhase::Bubble && ev.button == MouseButton::Left && hb.is_hovered(window) {
                    let mut s = st.borrow_mut();
                    let center = if s.zoom.is_some() { s.center } else { (pw as f32 / 2.0, ph as f32 / 2.0) };
                    s.drag = Some(Drag { start: ev.position, center0: center, moved: false });
                    window.capture_pointer(hb.id);
                }
            });

            let st = state.clone();
            let hover_cb = on_hover.clone();
            let hb = hitbox.clone();
            window.on_mouse_event(move |ev: &MouseMoveEvent, phase, window, cx| {
                if phase != DispatchPhase::Bubble {
                    return;
                }
                let mut s = st.borrow_mut();
                let Some(bounds) = s.last_bounds else { return };
                let scale = s.effective_zoom();
                if let Some(d) = s.drag.as_mut() {
                    let delta = ev.position - d.start;
                    if !d.moved && (f32::from(delta.x).abs() + f32::from(delta.y).abs()) > 3.0 {
                        d.moved = true;
                    }
                    if d.moved && ev.pressed_button == Some(MouseButton::Left) {
                        let c = (d.center0.0 - f32::from(delta.x) / scale, d.center0.1 - f32::from(delta.y) / scale);
                        s.zoom = Some(scale);
                        s.center = c;
                        window.refresh();
                    }
                }
                let hovered = if hb.is_hovered(window) {
                    let g = Geometry::new(&mut s, bounds, pw, ph);
                    source_pixel(&g, ev.position, pw, ph)
                } else {
                    None
                };
                if hovered != s.hover {
                    s.hover = hovered;
                    drop(s);
                    hover_cb(window, cx);
                }
            });

            let st = state.clone();
            let grid_up = grid.clone();
            let select = on_select.clone();
            window.on_mouse_event(move |ev: &MouseUpEvent, phase, window, cx| {
                if phase != DispatchPhase::Bubble || ev.button != MouseButton::Left {
                    return;
                }
                let mut s = st.borrow_mut();
                let Some(d) = s.drag.take() else { return };
                window.release_pointer();
                if d.moved {
                    return;
                }
                let Some(bounds) = s.last_bounds else { return };
                let g = Geometry::new(&mut s, bounds, pw, ph);
                let hit = source_pixel(&g, ev.position, pw, ph);
                drop(s);
                if let Some(grid) = &grid_up {
                    select(hit.and_then(|(x, y)| block_at(grid, x, y)), window, cx);
                }
            });

            let st = state.clone();
            let hb = hitbox.clone();
            window.on_mouse_event(move |ev: &ScrollEvent, phase, window, _| {
                if phase != DispatchPhase::Bubble || !hb.is_hovered(window) {
                    return;
                }
                let mut s = st.borrow_mut();
                let Some(bounds) = s.last_bounds else { return };
                let delta = ev.delta.pixel_delta(px(16.0));
                if ev.modifiers.platform || ev.modifiers.control {
                    zoom_at(&mut s, bounds, ev.position, (-f32::from(delta.y) * 0.01).exp(), pw, ph);
                } else {
                    let scale = s.effective_zoom();
                    if s.zoom.is_none() {
                        s.center = (pw as f32 / 2.0, ph as f32 / 2.0);
                    }
                    s.zoom = Some(scale);
                    s.center.0 -= f32::from(delta.x) / scale;
                    s.center.1 -= f32::from(delta.y) / scale;
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
                let Some(bounds) = s.last_bounds else { return };
                zoom_at(&mut s, bounds, ev.position, 1.0 + ev.delta, pw, ph);
                window.refresh();
            });
        },
    )
    .size_full()
}

fn zoom_at(s: &mut ViewerState, bounds: Bounds<Pixels>, at: Point<Pixels>, factor: f32, pw: usize, ph: usize) {
    let g = Geometry::new(s, bounds, pw, ph);
    let src = g.to_source(at);
    let z = (g.scale * factor).clamp(MIN_ZOOM, MAX_ZOOM);
    let c = bounds.center();
    s.zoom = Some(z);
    s.center = (src.0 - f32::from(at.x - c.x) / z, src.1 - f32::from(at.y - c.y) / z);
}
