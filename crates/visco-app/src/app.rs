//! Root view: state, loading, background encode/decode, and the overall layout.

use std::{cell::RefCell, ops::Range, path::PathBuf, rc::Rc, sync::Arc, time::Duration};

use gpui_kit::component::{
    ActiveTheme, Sizable, StyledExt,
    button::{Button, ButtonVariants},
    h_resizable, resizable_panel,
    slider::{Slider, SliderEvent, SliderState},
    v_resizable,
};
use gpui_kit::*;
use gpui_kit::base::{Disableable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use visco_core::{BlockId, BlockInfo, Codec, ParamSpec, ParamValue, Params, RgbImage, StageView, Trace};

use crate::{
    hexview,
    inspector::{self, InspectorTab},
    viewer::{self, ViewerProps, ViewerState},
};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    Decode,
    Encode,
}

#[derive(Default)]
pub struct Slot {
    pub trace: Option<Arc<Trace>>,
    pub error: Option<String>,
    pub busy: bool,
    task: Option<Task<()>>,
    generation: u64,
}

pub struct Source {
    pub name: String,
    /// Original bytes when the file is in the codec's own format.
    pub codec_bytes: Option<Arc<[u8]>>,
    /// Pixels for encoding.
    pub rgb: Option<RgbImage>,
}

pub struct Visco {
    pub codec: Arc<dyn Codec>,
    pub source: Option<Source>,
    pub load_error: Option<String>,
    loading: Option<Task<()>>,
    pub mode: Mode,
    pub decode: Slot,
    pub encode: Slot,
    stage_ix: [usize; 2],
    plane_ix: usize,
    pub selected_block: Option<BlockId>,
    pub block_info: Option<BlockInfo>,
    /// Byte ranges of the selected block's spans (merged, sorted).
    pub block_bytes: Vec<Range<usize>>,
    pub hover_byte: Option<usize>,
    pub selected_byte: Option<usize>,
    pub params: Params,
    pub param_sliders: Vec<(&'static str, Entity<SliderState>)>,
    scan_slider: Option<Entity<SliderState>>,
    scan_ix: usize,
    pub viewer: Rc<RefCell<ViewerState>>,
    pub hex_scroll: UniformListScrollHandle,
    pub tab: InspectorTab,
    _subs: Vec<Subscription>,
}

impl Visco {
    pub fn new(codec: Arc<dyn Codec>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let schema = codec.params_schema();
        let params = Params::defaults(&schema);
        let mut subs = vec![];
        let mut sliders = vec![];
        for spec in &schema {
            if let ParamSpec::Int { key, min, max, default, .. } = *spec {
                let s = cx.new(|_| {
                    SliderState::new().min(min as f32).max(max as f32).step(1.0).default_value(default as f32)
                });
                subs.push(cx.subscribe_in(&s, window, move |this, _, ev: &SliderEvent, _, cx| {
                    let SliderEvent::Change(v) = ev else { return };
                    let v = v.start().round() as i64;
                    if this.params.int(key, i64::MIN) != v {
                        this.params.set(key, ParamValue::Int(v));
                        this.schedule_encode(cx);
                    }
                }));
                sliders.push((key, s));
            }
        }
        Self {
            codec,
            source: None,
            load_error: None,
            loading: None,
            mode: Mode::Decode,
            decode: Slot::default(),
            encode: Slot::default(),
            stage_ix: [0, 0],
            plane_ix: 0,
            selected_block: None,
            block_info: None,
            block_bytes: vec![],
            hover_byte: None,
            selected_byte: None,
            params,
            param_sliders: sliders,
            scan_slider: None,
            scan_ix: 0,
            viewer: Rc::new(RefCell::new(ViewerState::default())),
            hex_scroll: UniformListScrollHandle::new(),
            tab: InspectorTab::Block,
            _subs: subs,
        }
    }

    pub fn trace(&self) -> Option<&Arc<Trace>> {
        self.slot(self.mode).trace.as_ref()
    }

    fn slot(&self, m: Mode) -> &Slot {
        match m {
            Mode::Decode => &self.decode,
            Mode::Encode => &self.encode,
        }
    }

    fn slot_mut(&mut self, m: Mode) -> &mut Slot {
        match m {
            Mode::Decode => &mut self.decode,
            Mode::Encode => &mut self.encode,
        }
    }

    fn stage_ix(&self) -> usize {
        self.stage_ix[self.mode as usize]
    }

    // ------------------------------------------------------------------ loading

    pub fn prompt_open(&mut self, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Open".into()),
        });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = rx.await
                && let Some(p) = paths.into_iter().next()
            {
                this.update(cx, |this, cx| this.open(p, cx)).ok();
            }
        })
        .detach();
    }

    pub fn open(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let codec = self.codec.clone();
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        self.load_error = None;
        self.loading = Some(cx.spawn(async move |this, cx| {
            let result = cx.background_spawn(async move { load_source(&*codec, &path) }).await;
            this.update(cx, |this, cx| {
                this.loading = None;
                match result {
                    Ok((codec_bytes, rgb)) => {
                        this.mode = if codec_bytes.is_some() { Mode::Decode } else { Mode::Encode };
                        this.source = Some(Source { name, codec_bytes, rgb });
                        this.decode = Slot::default();
                        this.encode = Slot::default();
                        this.stage_ix = [0, 0];
                        this.clear_selection();
                        this.viewer.borrow_mut().reset();
                        this.schedule_decode(None, cx);
                        this.schedule_encode(cx);
                    }
                    Err(e) => this.load_error = Some(format!("{name}: {e}")),
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn clear_selection(&mut self) {
        self.selected_block = None;
        self.block_info = None;
        self.block_bytes.clear();
        self.hover_byte = None;
        self.selected_byte = None;
        self.plane_ix = 0;
        self.scan_ix = 0;
    }

    /// Decode the source file, or (for non-codec sources) the given encoded bytes.
    fn schedule_decode(&mut self, encoded: Option<Arc<[u8]>>, cx: &mut Context<Self>) {
        let Some(src) = &self.source else { return };
        let Some(bytes) = src.codec_bytes.clone().or(encoded) else { return };
        let codec = self.codec.clone();
        let slot = &mut self.decode;
        slot.generation += 1;
        let generation = slot.generation;
        slot.busy = true;
        slot.task = Some(cx.spawn(async move |this, cx| {
            let result = cx.background_spawn(async move { codec.decode(bytes) }).await;
            this.update(cx, |this, cx| this.finish(Mode::Decode, generation, result, cx)).ok();
        }));
    }

    pub fn schedule_encode(&mut self, cx: &mut Context<Self>) {
        let Some(rgb) = self.source.as_ref().and_then(|s| s.rgb.clone()) else { return };
        let codec = self.codec.clone();
        let params = self.params.clone();
        let first = self.encode.trace.is_none();
        let slot = &mut self.encode;
        slot.generation += 1;
        let generation = slot.generation;
        slot.busy = true;
        // Replacing the task drops (cancels) any pending one, which debounces slider drags.
        slot.task = Some(cx.spawn(async move |this, cx| {
            if !first {
                cx.background_executor().timer(Duration::from_millis(90)).await;
            }
            let result = cx.background_spawn(async move { codec.encode(&rgb, &params) }).await;
            this.update(cx, |this, cx| this.finish(Mode::Encode, generation, result, cx)).ok();
        }));
        cx.notify();
    }

    fn finish(
        &mut self,
        mode: Mode,
        generation: u64,
        result: Result<Trace, visco_core::CodecError>,
        cx: &mut Context<Self>,
    ) {
        let slot = self.slot_mut(mode);
        if slot.generation != generation {
            return;
        }
        slot.busy = false;
        slot.task = None;
        match result {
            Ok(t) => {
                slot.trace = Some(Arc::new(t));
                slot.error = None;
            }
            Err(e) => {
                slot.trace = None;
                slot.error = Some(e.0);
            }
        }
        if mode == Mode::Encode && self.source.as_ref().is_some_and(|s| s.codec_bytes.is_none()) {
            let bytes = self.encode.trace.as_ref().map(|t| t.bitstream.bytes.clone());
            self.schedule_decode(bytes, cx);
        }
        if mode == self.mode {
            self.on_trace_changed(cx);
        }
        cx.notify();
    }

    fn on_trace_changed(&mut self, cx: &mut Context<Self>) {
        let Some(t) = self.trace().cloned() else { return };
        let n = t.stages.len();
        let ix = &mut self.stage_ix[self.mode as usize];
        *ix = (*ix).min(n.saturating_sub(1));
        // Keep the selected block across re-encodes so its detail updates live.
        let keep = self.selected_block.filter(|&b| t.blocks.as_ref().is_some_and(|d| b < d.block_count()));
        self.select_block(keep, false, cx);
        self.rebuild_scan_slider(cx);
    }

    pub fn set_mode(&mut self, mode: Mode, cx: &mut Context<Self>) {
        if self.mode != mode {
            self.mode = mode;
            self.hover_byte = None;
            self.selected_byte = None;
            self.plane_ix = 0;
            self.on_trace_changed(cx);
            cx.notify();
        }
    }

    fn select_stage(&mut self, ix: usize, cx: &mut Context<Self>) {
        self.stage_ix[self.mode as usize] = ix;
        self.plane_ix = 0;
        self.rebuild_scan_slider(cx);
        cx.notify();
    }

    fn rebuild_scan_slider(&mut self, cx: &mut Context<Self>) {
        let n = match self.current_view() {
            Some(StageView::Sequence(s)) => s.len(),
            _ => 0,
        };
        if n == 0 {
            self.scan_slider = None;
            return;
        }
        self.scan_ix = self.scan_ix.min(n - 1);
        let init = self.scan_ix as f32;
        let s = cx.new(|_| SliderState::new().min(0.0).max((n - 1).max(1) as f32).step(1.0).default_value(init));
        self._subs.push(cx.subscribe(&s, |this, _, ev: &SliderEvent, cx| {
            let SliderEvent::Change(v) = ev else { return };
            let ix = v.start().round() as usize;
            if ix != this.scan_ix {
                this.scan_ix = ix;
                this.scroll_to_scan(cx);
                cx.notify();
            }
        }));
        self.scan_slider = Some(s);
    }

    fn scroll_to_scan(&mut self, _cx: &mut Context<Self>) {
        let Some(t) = self.trace().cloned() else { return };
        if let Some(StageView::Sequence(snaps)) = self.current_view()
            && let Some(&seg) = snaps.get(self.scan_ix).and_then(|s| s.segments.first())
        {
            let start = t.bitstream.segments[seg].bytes.start;
            self.selected_byte = Some(start);
            self.hex_scroll.scroll_to_item(start / hexview::BYTES_PER_ROW, ScrollStrategy::Top);
        }
    }

    pub fn current_view(&self) -> Option<&StageView> {
        self.trace().and_then(|t| t.stages.get(self.stage_ix())).map(|s| &s.view)
    }

    // ------------------------------------------------------------------ selection

    pub fn select_block(&mut self, id: Option<BlockId>, scroll: bool, cx: &mut Context<Self>) {
        self.selected_block = id;
        self.block_info = None;
        self.block_bytes.clear();
        if let (Some(id), Some(t)) = (id, self.trace().cloned())
            && let Some(blocks) = &t.blocks
        {
            let info = blocks.block_info(id, &t.bitstream);
            let mut ranges: Vec<Range<usize>> = vec![];
            for &si in &info.spans {
                let s = &t.bitstream.spans[si];
                let r = (s.start / 8) as usize..s.end().div_ceil(8) as usize;
                match ranges.last_mut() {
                    Some(last) if last.end >= r.start => last.end = last.end.max(r.end),
                    _ => ranges.push(r),
                }
            }
            if scroll && let Some(first) = ranges.first() {
                self.hex_scroll.scroll_to_item(first.start / hexview::BYTES_PER_ROW, ScrollStrategy::Center);
            }
            self.block_bytes = ranges;
            self.block_info = Some(info);
            if scroll {
                self.tab = InspectorTab::Block;
            }
        }
        cx.notify();
    }

    /// Select the block containing a bit span and bring it into view.
    pub fn select_span(&mut self, span_ix: usize, cx: &mut Context<Self>) {
        let Some(t) = self.trace().cloned() else { return };
        let Some(span) = t.bitstream.spans.get(span_ix) else { return };
        self.selected_byte = Some((span.start / 8) as usize);
        if let Some(b) = span.block_id() {
            self.select_block(Some(b), false, cx);
            self.focus_block(b);
        }
        cx.notify();
    }

    /// Center the viewer on a block, switching to a plane that shows it if needed.
    fn focus_block(&mut self, id: BlockId) {
        let Some(StageView::Planes(pvs)) = self.current_view() else { return };
        let found = std::iter::once(self.plane_ix)
            .chain(0..pvs.len())
            .filter_map(|i| {
                let g = pvs.get(i)?.grid.as_ref()?;
                viewer::block_cell(g, id).map(|c| (i, c, g.block_w, g.block_h, pvs[i].plane.width, pvs[i].plane.height))
            })
            .next();
        if let Some((i, (c, r), bw, bh, pw, ph)) = found {
            self.plane_ix = i;
            self.viewer.borrow_mut().focus((c * bw) as f32, (r * bh) as f32, bw as f32, bh as f32, (pw, ph));
        }
    }

    pub fn hover_span(&self) -> Option<usize> {
        let t = self.trace()?;
        self.hover_byte.and_then(|b| t.bitstream.span_in_byte(b))
    }

    // ------------------------------------------------------------------ rendering

    fn render_toolbar(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let has_rgb = self.source.as_ref().is_some_and(|s| s.rgb.is_some());
        let decode_label = if self.source.as_ref().is_some_and(|s| s.codec_bytes.is_none()) {
            "Decode (visco output)"
        } else {
            "Decode"
        };
        let busy = self.slot(self.mode).busy || self.loading.is_some();
        h_flex()
            .gap_2()
            .px_3()
            .py_2()
            .border_b_1()
            .border_color(theme.border)
            .bg(theme.title_bar)
            .child(Button::new("open").label("Open…").small().on_click(cx.listener(|this, _, _, cx| this.prompt_open(cx))))
            .child(
                div()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child(self.source.as_ref().map_or("No file — open or drop an image or JPEG".to_string(), |s| s.name.clone())),
            )
            .child(div().flex_1())
            .when(self.source.is_some(), |el| {
                el.child(
                    Button::new("mode-decode")
                        .label(decode_label)
                        .small()
                        .when(self.mode == Mode::Decode, |b| b.primary())
                        .when(self.mode != Mode::Decode, |b| b.ghost())
                        .on_click(cx.listener(|this, _, _, cx| this.set_mode(Mode::Decode, cx))),
                )
                .child(
                    Button::new("mode-encode")
                        .label("Encode")
                        .small()
                        .disabled(!has_rgb)
                        .when(self.mode == Mode::Encode, |b| b.primary())
                        .when(self.mode != Mode::Encode, |b| b.ghost())
                        .on_click(cx.listener(|this, _, _, cx| this.set_mode(Mode::Encode, cx))),
                )
            })
            .child(
                div()
                    .w(px(90.))
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(if busy { "working…" } else { "" }),
            )
            .child(div().text_sm().font_semibold().child(self.codec.name()))
            .into_any_element()
    }

    fn render_stage_list(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let current = self.stage_ix();
        let mut list = v_flex().id("stages").size_full().overflow_y_scroll().p_2().gap_1();
        if let Some(t) = self.trace() {
            for (i, st) in t.stages.iter().enumerate() {
                let selected = i == current;
                list = list.child(
                    div()
                        .id(("stage", i))
                        .px_2()
                        .py_1p5()
                        .rounded_md()
                        .text_sm()
                        .cursor_pointer()
                        .when(selected, |d| d.bg(theme.list_active).border_1().border_color(theme.list_active_border))
                        .when(!selected, |d| d.hover(|d| d.bg(theme.list_hover)))
                        .child(
                            h_flex()
                                .gap_2()
                                .child(div().w(px(18.)).text_color(theme.muted_foreground).child(format!("{}", i + 1)))
                                .child(st.title.clone()),
                        )
                        .on_click(cx.listener(move |this, _, _, cx| this.select_stage(i, cx))),
                );
            }
        }
        v_flex()
            .size_full()
            .bg(theme.sidebar)
            .child(
                div()
                    .px_3()
                    .pt_3()
                    .pb_1()
                    .text_xs()
                    .font_semibold()
                    .text_color(theme.muted_foreground)
                    .child(match self.mode {
                        Mode::Decode => "DECODING PIPELINE",
                        Mode::Encode => "ENCODING PIPELINE",
                    }),
            )
            .child(list)
            .into_any_element()
    }

    fn render_stage(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let slot = self.slot(self.mode);
        let Some(trace) = slot.trace.clone() else {
            let msg = if let Some(e) = &self.load_error {
                e.clone()
            } else if let Some(e) = &slot.error {
                e.clone()
            } else if self.source.is_none() {
                "Open a JPEG to step through decoding, or any image (PNG, JPEG, BMP) to encode it.\n\
                 Drag a file onto the window, or press ⌘O."
                    .into()
            } else {
                "Working…".into()
            };
            return div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .p_8()
                .text_color(theme.muted_foreground)
                .child(msg)
                .into_any_element();
        };
        let Some(stage) = trace.stages.get(self.stage_ix()) else { return div().into_any_element() };

        let header = v_flex()
            .px_4()
            .pt_3()
            .pb_2()
            .gap_1()
            .child(div().text_lg().font_semibold().child(stage.title.clone()))
            .child(div().text_sm().text_color(theme.muted_foreground).child(stage.description.clone()));

        let body: AnyElement = match &stage.view {
            StageView::Planes(pvs) => {
                let pi = self.plane_ix.min(pvs.len().saturating_sub(1));
                let pv = &pvs[pi];
                let mut chooser = h_flex().gap_1().flex_wrap();
                if pvs.len() > 1 {
                    for (i, p) in pvs.iter().enumerate() {
                        chooser = chooser.child(
                            Button::new(("plane", i))
                                .label(p.plane.label.clone())
                                .xsmall()
                                .when(i == pi, |b| b.primary())
                                .when(i != pi, |b| b.ghost())
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.plane_ix = i;
                                    cx.notify();
                                })),
                        );
                    }
                }
                let highlight = self.hover_span().and_then(|s| trace.bitstream.spans[s].block_id());
                v_flex()
                    .size_full()
                    .child(h_flex().px_4().pb_2().gap_3().child(chooser).child(div().flex_1()).child(self.zoom_controls(pv.plane.width, pv.plane.height, cx)))
                    .child(div().flex_1().min_h_0().child(self.make_viewer(pv.plane.clone(), pv.grid.clone(), highlight, cx)))
                    .child(self.readout(&pv.plane, pv.grid.as_ref(), &theme))
                    .into_any_element()
            }
            StageView::Sequence(snaps) if !snaps.is_empty() => {
                let si = self.scan_ix.min(snaps.len() - 1);
                let snap = &snaps[si];
                v_flex()
                    .size_full()
                    .child(
                        h_flex()
                            .px_4()
                            .pb_2()
                            .gap_3()
                            .child(div().font_semibold().child(snap.label.clone()))
                            .child(div().text_sm().text_color(theme.muted_foreground).child(snap.detail.clone()))
                            .child(div().flex_1())
                            .child(self.zoom_controls(snap.plane.width, snap.plane.height, cx)),
                    )
                    .when_some(self.scan_slider.clone(), |el, s| el.child(div().px_4().pb_2().child(Slider::new(&s))))
                    .child(div().flex_1().min_h_0().child(self.make_viewer(snap.plane.clone(), None, None, cx)))
                    .into_any_element()
            }
            StageView::Sequence(_) => div().p_4().child("No snapshots.").into_any_element(),
            StageView::Tables(tables) => inspector::render_tables(tables, &theme).into_any_element(),
            StageView::Text(text) => div()
                .id("stage-text")
                .size_full()
                .overflow_y_scroll()
                .px_4()
                .pb_4()
                .font_family(theme.mono_font_family.clone())
                .text_xs()
                .whitespace_normal()
                .children(text.lines().map(|l| div().min_h(px(14.)).child(l.to_string())))
                .into_any_element(),
        };
        v_flex().size_full().child(header).child(div().flex_1().min_h_0().child(body)).into_any_element()
    }

    fn make_viewer(
        &self,
        plane: visco_core::Plane,
        grid: Option<visco_core::BlockGrid>,
        highlight: Option<BlockId>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let entity = cx.entity();
        let e2 = entity.clone();
        viewer::viewer(ViewerProps {
            plane,
            grid,
            selected: self.selected_block,
            highlight,
            state: self.viewer.clone(),
            on_select: Rc::new(move |id, _, cx| {
                entity.update(cx, |this, cx| {
                    this.select_block(id, true, cx);
                });
            }),
            on_hover: Rc::new(move |_, cx| e2.update(cx, |_, cx| cx.notify())),
        })
        .into_any_element()
    }

    fn zoom_controls(&self, w: usize, h: usize, cx: &mut Context<Self>) -> AnyElement {
        let z = self.viewer.borrow().effective_zoom();
        let fitting = self.viewer.borrow().zoom.is_none();
        h_flex()
            .gap_1()
            .child(
                Button::new("zoom-fit").label("Fit").xsmall().when(fitting, |b| b.primary()).when(!fitting, |b| b.ghost()).on_click(
                    cx.listener(|this, _, _, cx| {
                        this.viewer.borrow_mut().reset();
                        cx.notify();
                    }),
                ),
            )
            .child(Button::new("zoom-1").label("1:1").xsmall().ghost().on_click(cx.listener(move |this, _, _, cx| {
                this.viewer.borrow_mut().set_zoom(1.0, (w, h));
                cx.notify();
            })))
            .child(Button::new("zoom-out").label("−").xsmall().ghost().on_click(cx.listener(move |this, _, _, cx| {
                this.viewer.borrow_mut().zoom_by(1.0 / 1.5, (w, h));
                cx.notify();
            })))
            .child(Button::new("zoom-in").label("+").xsmall().ghost().on_click(cx.listener(move |this, _, _, cx| {
                this.viewer.borrow_mut().zoom_by(1.5, (w, h));
                cx.notify();
            })))
            .child(div().w(px(56.)).text_xs().text_right().child(format!("{:.0}%", z * 100.0)))
            .into_any_element()
    }

    fn readout(&self, plane: &visco_core::Plane, grid: Option<&visco_core::BlockGrid>, theme: &gpui_kit::component::Theme) -> AnyElement {
        let hover = self.viewer.borrow().hover;
        let text = match hover {
            Some((x, y)) if x < plane.width && y < plane.height => {
                let i = y * plane.width + x;
                let v = match &plane.kind {
                    visco_core::PlaneKind::Gray(d) => format!("{}", d[i]),
                    visco_core::PlaneKind::Rgb(d) => format!("RGB({}, {}, {})", d[3 * i], d[3 * i + 1], d[3 * i + 2]),
                    visco_core::PlaneKind::Scalar { data, .. } => format!("{:.2}", data[i]),
                };
                let block = grid.map(|g| format!(" · block ({}, {})", x / g.block_w, y / g.block_h)).unwrap_or_default();
                format!("{} · ({x}, {y}) = {v}{block}", plane.label)
            }
            _ => format!("{} · {}×{} · drag to pan, ⌘-scroll or pinch to zoom, click a block to inspect", plane.label, plane.width, plane.height),
        };
        div()
            .px_4()
            .py_1()
            .text_xs()
            .text_color(theme.muted_foreground)
            .font_family(theme.mono_font_family.clone())
            .child(text)
            .into_any_element()
    }
}

impl Render for Visco {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let toolbar = self.render_toolbar(cx);
        let stages = self.render_stage_list(cx);
        let stage = self.render_stage(cx);
        let hex = hexview::render(self, window, cx);
        let inspector = inspector::render(self, cx);
        v_flex()
            .size_full()
            .bg(theme.background)
            .text_color(theme.foreground)
            .on_drop(cx.listener(|this, paths: &ExternalPaths, _, cx| {
                if let Some(p) = paths.paths().first() {
                    this.open(p.clone(), cx);
                }
            }))
            .child(toolbar)
            .child(
                div().flex_1().min_h_0().child(
                    h_resizable("main")
                        .child(resizable_panel().size(px(230.)).size_range(px(160.)..px(400.)).child(stages))
                        .child(
                            resizable_panel().child(
                                v_resizable("center")
                                    .child(resizable_panel().child(stage))
                                    .child(resizable_panel().size(px(230.)).size_range(px(80.)..px(700.)).child(hex)),
                            ),
                        )
                        .child(resizable_panel().size(px(380.)).size_range(px(260.)..px(800.)).child(inspector)),
                ),
            )
    }
}

/// Read a file: codec-format files keep their bytes for decoding; any decodable image yields RGB.
type Loaded = (Option<Arc<[u8]>>, Option<RgbImage>);

fn load_source(codec: &dyn Codec, path: &std::path::Path) -> Result<Loaded, String> {
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let ext = path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
    let is_codec = codec.extensions().contains(&ext.as_str())
        || (codec.id() == "jpeg" && bytes.starts_with(&[0xFF, 0xD8]));
    let bytes: Arc<[u8]> = bytes.into();
    if is_codec {
        // Our own decoder supplies the pixels for re-encoding (the image crate mis-decodes some files).
        let rgb = visco_jpeg::decoder::decode(&bytes, visco_jpeg::decoder::DecodeOptions {
            record_spans: false,
            snapshots: false,
            ..Default::default()
        })
        .ok()
        .map(|d| RgbImage { width: d.frame.width as usize, height: d.frame.height as usize, data: d.rgb.into() });
        return Ok((Some(bytes), rgb));
    }
    let img = image::load_from_memory(&bytes).map_err(|e| format!("not a supported image: {e}"))?.to_rgb8();
    let (w, h) = (img.width() as usize, img.height() as usize);
    Ok((None, Some(RgbImage { width: w, height: h, data: img.into_raw().into() })))
}
