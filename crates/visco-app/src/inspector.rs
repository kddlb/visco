//! Right-hand inspector: block detail, segment fields, live encoder parameters, metrics.

use gpui_kit::component::{
    ActiveTheme, Sizable, StyledExt, Theme,
    button::{Button, ButtonVariants},
    slider::Slider,
    switch::Switch,
};
use gpui_kit::*;
use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use visco_core::{DetailGrid, DetailPlot, ParamSpec, ParamValue, Ramp, Table};

use crate::{
    app::{Mode, Visco},
    raster,
};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum InspectorTab {
    Block,
    Segment,
    Params,
    Info,
}

pub fn render(this: &mut Visco, cx: &mut Context<Visco>) -> AnyElement {
    let theme = cx.theme().clone();
    let can_encode = this.can_encode();
    let mut tabs = h_flex().gap_1().px_2().py_2().border_b_1().border_color(theme.border);
    for (tab, label) in [
        (InspectorTab::Block, "Block"),
        (InspectorTab::Segment, "Segment"),
        (InspectorTab::Params, "Encoder"),
        (InspectorTab::Info, "Info"),
    ] {
        if tab == InspectorTab::Params && !can_encode {
            continue;
        }
        tabs = tabs.child(
            Button::new(label)
                .label(label)
                .small()
                .when(this.tab == tab, |b| b.primary())
                .when(this.tab != tab, |b| b.ghost())
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.tab = tab;
                    cx.notify();
                })),
        );
    }
    let body: AnyElement = match this.tab {
        InspectorTab::Block => block_tab(this, &theme, cx).into_any_element(),
        InspectorTab::Segment => segment_tab(this, &theme).into_any_element(),
        InspectorTab::Params if can_encode => params_tab(this, &theme, cx).into_any_element(),
        _ => info_tab(this, &theme).into_any_element(),
    };
    v_flex()
        .size_full()
        .bg(theme.sidebar)
        .border_l_1()
        .border_color(theme.border)
        .child(tabs)
        .child(div().id("inspector-body").flex_1().min_h_0().overflow_y_scroll().p_3().child(body))
        .into_any_element()
}

fn kv(k: impl Into<SharedString>, v: impl Into<SharedString>, theme: &Theme) -> impl IntoElement {
    h_flex()
        .gap_2()
        .text_sm()
        .child(div().w(px(140.)).flex_none().text_color(theme.muted_foreground).child(k.into()))
        .child(div().flex_1().child(v.into()))
}

fn hint(text: &'static str, theme: &Theme) -> impl IntoElement {
    div().text_sm().text_color(theme.muted_foreground).child(text)
}

fn block_tab(this: &Visco, theme: &Theme, cx: &mut Context<Visco>) -> impl IntoElement {
    let Some(info) = &this.block_info else {
        return v_flex().child(hint("Click a block in the viewer (or a byte of entropy-coded data) to inspect it.", theme));
    };
    let mut col = v_flex().gap_2().child(div().text_lg().font_semibold().child(info.title.clone()));
    for (k, v) in &info.summary {
        col = col.child(kv(k.clone(), v.clone(), theme));
    }
    let mut grids = h_flex().flex_wrap().gap_3().pt_2();
    for g in &info.grids {
        grids = grids.child(detail_grid(g, theme));
    }
    col = col.child(grids);
    for p in &info.plots {
        col = col.child(detail_plot(p, theme));
    }
    for t in &info.tables {
        col = col.child(compact_table(t, theme));
    }

    if let Some(trace) = this.trace() {
        let spans = &info.spans;
        if !spans.is_empty() {
            col = col.child(div().pt_2().font_semibold().text_sm().child(format!("Coded symbols ({})", spans.len())));
            let hover = this.hover_span();
            for &si in spans.iter().take(400) {
                let s = trace.bitstream.spans[si];
                let bits = bit_string(&trace.bitstream.bytes, s.start, s.len);
                col = col.child(
                    h_flex()
                        .id(("span", si))
                        .gap_2()
                        .px_1()
                        .rounded_sm()
                        .text_xs()
                        .font_family(theme.mono_font_family.clone())
                        .cursor_pointer()
                        .when(hover == Some(si), |d| d.bg(hsla(0.14, 1.0, 0.55, 0.35)))
                        .hover(|d| d.bg(theme.list_hover))
                        .child(div().w(px(150.)).flex_none().child(s.kind.describe()))
                        .child(div().text_color(theme.muted_foreground).child(bits))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            let byte = (s.start / 8) as usize;
                            this.hover_byte = Some(byte);
                            this.selected_byte = Some(byte);
                            this.hex_scroll.scroll_to_item(byte / crate::hexview::BYTES_PER_ROW, ScrollStrategy::Center);
                            cx.notify();
                        })),
                );
            }
            if spans.len() > 400 {
                col = col.child(hint("…", theme));
            }
        }
    }
    col
}

/// The bits of a span as a 0/1 string (capped).
fn bit_string(bytes: &[u8], start: u64, len: u32) -> String {
    (start..start + (len as u64).min(64))
        .map(|b| if (bytes[(b / 8) as usize] >> (7 - b % 8)) & 1 == 1 { '1' } else { '0' })
        .collect()
}

/// A small line chart: min/max per pixel column, markers as vertical ticks.
fn detail_plot(p: &DetailPlot, theme: &Theme) -> impl IntoElement {
    let series = p.series.clone();
    let markers: Vec<usize> = p.markers.iter().map(|m| m.0).collect();
    let n = series.iter().map(|s| s.data.len()).max().unwrap_or(0);
    let (lo, hi) = series
        .iter()
        .flat_map(|s| s.data.iter())
        .fold((f32::MAX, f32::MIN), |(a, b), &v| (a.min(v), b.max(v)));
    let (lo, hi) = if lo > hi { (-1.0, 1.0) } else if hi - lo < 1e-6 { (lo - 1.0, hi + 1.0) } else { (lo, hi) };
    let chart = canvas(
        |_, _, _| {},
        move |bounds, _, window, _| {
            window.paint_quad(fill(bounds, rgb(0x16161a)));
            if n == 0 {
                return;
            }
            let w = f32::from(bounds.size.width);
            let h = f32::from(bounds.size.height) - 4.0;
            let y_of = |v: f32| bounds.top() + px(2.0 + (hi - v) / (hi - lo) * h);
            if lo < 0.0 && hi > 0.0 {
                window.paint_quad(fill(Bounds { origin: point(bounds.left(), y_of(0.0)), size: size(bounds.size.width, px(1.0)) }, hsla(0.0, 0.0, 1.0, 0.12)));
            }
            for &m in &markers {
                let x = bounds.left() + px(m as f32 / n as f32 * w);
                window.paint_quad(fill(Bounds { origin: point(x, bounds.top()), size: size(px(1.0), bounds.size.height) }, hsla(0.0, 0.0, 1.0, 0.22)));
            }
            let cols = (w * window.scale_factor()) as usize;
            for s in &series {
                let color = crate::signal_viewer::role_color(s.role);
                let d = &s.data;
                if d.len() * 2 <= cols {
                    let mut path = PathBuilder::stroke(px(1.2));
                    for (i, &v) in d.iter().enumerate() {
                        let pt = point(bounds.left() + px((i as f32 + 0.5) / d.len() as f32 * w), y_of(v));
                        if i == 0 { path.move_to(pt) } else { path.line_to(pt) }
                    }
                    if let Ok(path) = path.build() {
                        window.paint_path(path, color);
                    }
                } else {
                    for c in 0..cols {
                        let (a, b) = (c * d.len() / cols, ((c + 1) * d.len() / cols).max(c * d.len() / cols + 1).min(d.len()));
                        let (mn, mx) = d[a..b].iter().fold((f32::MAX, f32::MIN), |(x, y), &v| (x.min(v), y.max(v)));
                        let x = bounds.left() + px(c as f32 / window.scale_factor());
                        let (t, btm) = (y_of(mx), y_of(mn));
                        window.paint_quad(fill(Bounds { origin: point(x, t), size: size(px(1.0 / window.scale_factor()), (btm - t).max(px(1.0))) }, color));
                    }
                }
            }
        },
    )
    .w_full()
    .h(px(110.));
    let mut legend = h_flex().gap_3().text_xs();
    for s in &p.series {
        legend = legend.child(
            h_flex()
                .gap_1()
                .child(div().w(px(10.)).h(px(3.)).bg(crate::signal_viewer::role_color(s.role)))
                .child(div().text_color(theme.muted_foreground).child(s.label.clone())),
        );
    }
    v_flex()
        .gap_1()
        .pt_2()
        .child(h_flex().gap_3().child(div().text_xs().font_semibold().child(p.title.clone())).child(legend))
        .child(chart)
        .child(div().text_xs().text_color(theme.muted_foreground).child(format!("{} samples · range {lo:.0} … {hi:.0}", n)))
}

fn compact_table(t: &Table, theme: &Theme) -> impl IntoElement {
    let cols = t.columns.len().max(1);
    let mut grid = v_flex().border_1().border_color(theme.border).rounded_md().text_xs().font_family(theme.mono_font_family.clone());
    let row = |cells: &[String], head: bool, odd: bool| {
        let mut r = h_flex().when(head, |d| d.bg(theme.table_head).font_semibold()).when(odd, |d| d.bg(theme.table_even));
        for (i, c) in cells.iter().enumerate() {
            r = r.child(div().when(i == 0, |d| d.flex_1()).when(i > 0, |d| d.w(px(if cols > 3 { 64. } else { 84. }))).px_2().py(px(1.)).child(c.clone()));
        }
        r
    };
    grid = grid.child(row(&t.columns, true, false));
    for (i, r) in t.rows.iter().enumerate() {
        grid = grid.child(row(r, false, i % 2 == 1));
    }
    v_flex().gap_1().pt_2().child(div().text_xs().font_semibold().child(t.title.clone())).child(grid)
}

fn detail_grid(g: &DetailGrid, theme: &Theme) -> impl IntoElement {
    let range = match g.ramp {
        Ramp::Gray => (0.0, 255.0),
        Ramp::Diverging => {
            let m = g.values.iter().fold(1e-6f32, |m, v| m.max(v.abs()));
            (-m, m)
        }
        Ramp::Heat => g.values.iter().fold((f32::MAX, f32::MIN), |(a, b), &v| (a.min(v), b.max(v))),
    };
    let cell = 37.0;
    let mut rows = v_flex().border_1().border_color(theme.border);
    for r in 0..g.values.len().div_ceil(g.width) {
        let mut row = h_flex();
        for c in 0..g.width {
            let Some(&v) = g.values.get(r * g.width + c) else { break };
            let bg = raster::ramp_rgb(g.ramp, v, range);
            let text = if g.precision == 0 { format!("{}", v.round() as i64) } else { format!("{v:.*}", g.precision) };
            row = row.child(
                div()
                    .w(px(cell))
                    .h(px(22.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_size(px(10.))
                    .bg(rgb(((bg[0] as u32) << 16) | ((bg[1] as u32) << 8) | bg[2] as u32))
                    .text_color(raster::contrast_text(bg))
                    .child(text),
            );
        }
        rows = rows.child(row);
    }
    v_flex()
        .gap_1()
        .child(div().text_xs().font_semibold().child(g.title.clone()))
        .child(rows)
        .font_family(theme.mono_font_family.clone())
}

fn segment_tab(this: &Visco, theme: &Theme) -> impl IntoElement {
    let seg = this
        .trace()
        .zip(this.selected_byte)
        .and_then(|(t, b)| t.bitstream.segment_at(b).map(|i| t.bitstream.segments[i].clone()));
    let Some(seg) = seg else {
        return v_flex().child(hint("Click a byte in the bitstream panel to see the segment it belongs to.", theme));
    };
    let mut col = v_flex()
        .gap_2()
        .child(div().text_lg().font_semibold().child(seg.name.clone()))
        .child(kv("bytes", format!("{}..{} ({} bytes)", seg.bytes.start, seg.bytes.end, seg.bytes.len()), theme));
    for (k, v) in seg.fields {
        col = col.child(kv(k, v, theme));
    }
    col
}

fn params_tab(this: &Visco, theme: &Theme, cx: &mut Context<Visco>) -> impl IntoElement {
    let mut col = v_flex().gap_4().child(hint(
        "Changes re-encode the image in the background. The selected block keeps its place, so its detail updates live.",
        theme,
    ));
    if this.mode != Mode::Encode {
        col = col.child(
            Button::new("to-encode")
                .label("Show the encoding pipeline")
                .small()
                .on_click(cx.listener(|this, _, _, cx| this.set_mode(Mode::Encode, cx))),
        );
    }
    for spec in this.codec.params_schema() {
        match spec {
            ParamSpec::Int { key, label, .. } => {
                let value = this.params.int(key, 0);
                let shown = if key == "restart" && value == 0 { "off".to_string() } else { value.to_string() };
                let mut c = v_flex().gap_1().child(
                    h_flex().child(div().flex_1().text_sm().child(label)).child(div().text_sm().font_semibold().child(shown)),
                );
                if let Some((_, s)) = this.param_sliders.iter().find(|(k, _)| *k == key) {
                    c = c.child(Slider::new(s));
                }
                col = col.child(c);
            }
            ParamSpec::Choice { key, label, options, .. } => {
                let cur = this.params.choice(key, 0);
                let mut row = h_flex().gap_1();
                for (i, opt) in options.iter().enumerate() {
                    row = row.child(
                        Button::new((key, i))
                            .label(*opt)
                            .small()
                            .when(i == cur, |b| b.primary())
                            .when(i != cur, |b| b.ghost())
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.params.set(key, ParamValue::Choice(i));
                                this.schedule_encode(cx);
                            })),
                    );
                }
                col = col.child(v_flex().gap_1().child(div().text_sm().child(label)).child(row));
            }
            ParamSpec::Bool { key, label, .. } => {
                let on = this.params.bool(key, false);
                let entity = cx.entity();
                col = col.child(Switch::new(key).label(label).checked(on).on_change(move |v, _, cx| {
                    let v = *v;
                    entity.update(cx, |this, cx| {
                        this.params.set(key, ParamValue::Bool(v));
                        this.schedule_encode(cx);
                    });
                }));
            }
        }
    }
    if let Some(t) = &this.encode.trace {
        col = col.child(v_flex().gap_1().pt_2().children(t.metrics.iter().map(|(k, v)| kv(k.clone(), v.clone(), theme))));
    }
    if let Some(e) = &this.encode.error {
        col = col.child(div().text_sm().text_color(theme.danger).child(e.clone()));
    }
    col
}

fn info_tab(this: &Visco, theme: &Theme) -> impl IntoElement {
    let Some(t) = this.trace() else { return v_flex().child(hint("Nothing loaded.", theme)) };
    let mut col = v_flex().gap_2();
    for (k, v) in &t.metrics {
        col = col.child(kv(k.clone(), v.clone(), theme));
    }
    col
}

pub fn render_tables(tables: &[Table], theme: &Theme) -> impl IntoElement {
    let mut col = v_flex().id("tables").size_full().overflow_y_scroll().px_4().pb_4().gap_4();
    for t in tables {
        let mut grid = v_flex().border_1().border_color(theme.border).rounded_md();
        let w = 76.0f32.max(if t.columns.len() <= 4 { 120.0 } else { 0.0 });
        let mut head = h_flex().bg(theme.table_head).text_color(theme.table_head_foreground);
        for c in &t.columns {
            head = head.child(div().w(px(w)).px_2().py_1().font_semibold().child(c.clone()));
        }
        grid = grid.child(head);
        for (i, r) in t.rows.iter().enumerate() {
            let mut row = h_flex().when(i % 2 == 1, |d| d.bg(theme.table_even));
            for c in r {
                row = row.child(div().w(px(w)).px_2().child(c.clone()));
            }
            grid = grid.child(row);
        }
        col = col.child(
            v_flex()
                .gap_1()
                .child(div().font_semibold().child(t.title.clone()))
                .child(grid.font_family(theme.mono_font_family.clone()).text_xs()),
        );
    }
    col
}
