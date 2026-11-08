//! Virtualized hex view of the bitstream with segment coloring and span ↔ block linking.

use std::ops::Range;

use gpui_kit::component::{ActiveTheme, StyledExt};
use gpui_kit::*;
use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use visco_core::Bitstream;

use crate::{app::Visco, inspector::InspectorTab};

pub const BYTES_PER_ROW: usize = 16;
const ROW_H: f32 = 18.0;

fn in_ranges(ranges: &[Range<usize>], b: usize) -> bool {
    let i = ranges.partition_point(|r| r.end <= b);
    i < ranges.len() && ranges[i].start <= b
}

fn segment_ix(bs: &Bitstream, byte: usize) -> Option<usize> {
    let i = bs.segments.partition_point(|s| s.bytes.start <= byte);
    (i > 0 && bs.segments[i - 1].bytes.contains(&byte)).then(|| i - 1)
}

fn is_ecs(name: &str) -> bool {
    name == "Entropy-coded data"
}

pub fn render(this: &mut Visco, _window: &mut Window, cx: &mut Context<Visco>) -> AnyElement {
    let theme = cx.theme().clone();
    let Some(trace) = this.trace().cloned() else {
        return div().size_full().bg(theme.background).into_any_element();
    };
    let bs = &trace.bitstream;
    let rows = bs.bytes.len().div_ceil(BYTES_PER_ROW);
    let hover_span = this.hover_span();

    let list = uniform_list(
        "hex",
        rows,
        cx.processor(move |this: &mut Visco, range: Range<usize>, _window, cx| {
            let Some(trace) = this.trace().cloned() else { return vec![] };
            let theme = cx.theme().clone();
            let bs = &trace.bitstream;
            let hover = this.hover_span().map(|s| {
                let sp = &bs.spans[s];
                (sp.start / 8) as usize..sp.end().div_ceil(8) as usize
            });
            let selected_seg = this.selected_byte.and_then(|b| segment_ix(bs, b));
            range
                .map(|r| {
                    let start = r * BYTES_PER_ROW;
                    let end = (start + BYTES_PER_ROW).min(bs.bytes.len());
                    let mut row = h_flex()
                        .h(px(ROW_H))
                        .px_2()
                        .child(div().w(px(76.)).text_color(theme.muted_foreground).child(format!("{start:08X}")));
                    let mut ascii = String::with_capacity(BYTES_PER_ROW);
                    for b in start..end {
                        let v = bs.bytes[b];
                        ascii.push(if v.is_ascii_graphic() { v as char } else { '·' });
                        let seg = segment_ix(bs, b);
                        let header = seg.is_some_and(|s| !is_ecs(&bs.segments[s].name));
                        let marker_byte = seg.is_some_and(|s| header && b < bs.segments[s].bytes.start + 2);
                        let in_block = in_ranges(&this.block_bytes, b);
                        let in_hover = hover.as_ref().is_some_and(|h| h.contains(&b));
                        let is_hovered = this.hover_byte == Some(b);
                        let seg_selected = seg.is_some() && seg == selected_seg && header;
                        let cell = div()
                            .w(px(22.))
                            .h(px(ROW_H - 2.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded_sm()
                            .when(b % 8 == 0 && b % BYTES_PER_ROW != 0, |d| d.ml_1())
                            .when(header, |d| d.text_color(hsla(0.09, 0.85, 0.55, 1.0)))
                            .when(marker_byte, |d| d.font_bold())
                            .when(seg_selected, |d| d.bg(hsla(0.09, 0.85, 0.55, 0.15)))
                            .when(in_block, |d| d.bg(hsla(0.52, 0.9, 0.5, 0.28)))
                            .when(in_hover, |d| d.bg(hsla(0.14, 1.0, 0.55, 0.45)))
                            .when(is_hovered, |d| d.border_1().border_color(theme.foreground))
                            .child(format!("{v:02X}"))
                            .on_mouse_move(cx.listener(move |this, _, _, cx| {
                                if this.hover_byte != Some(b) {
                                    this.hover_byte = Some(b);
                                    cx.notify();
                                }
                            }))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _, _, cx| {
                                    this.selected_byte = Some(b);
                                    let span = this.trace().and_then(|t| t.bitstream.span_in_byte(b));
                                    match span {
                                        Some(s) => this.select_span(s, cx),
                                        None => this.tab = InspectorTab::Segment,
                                    }
                                    cx.notify();
                                }),
                            );
                        row = row.child(cell);
                    }
                    row.child(div().ml_3().text_color(theme.muted_foreground).child(ascii))
                })
                .collect()
        }),
    )
    .track_scroll(&this.hex_scroll)
    .flex_1()
    .font_family(theme.mono_font_family.clone())
    .text_xs();

    let header = h_flex()
        .px_3()
        .py_1()
        .gap_3()
        .border_b_1()
        .border_color(theme.border)
        .text_xs()
        .child(div().font_semibold().child("Bitstream"))
        .child(div().text_color(theme.muted_foreground).child(format!(
            "{} bytes · {} segments · {} coded units",
            bs.bytes.len(),
            bs.segments.len(),
            bs.spans.len()
        )))
        .child(div().flex_1())
        .child(div().text_color(hsla(0.09, 0.85, 0.55, 1.0)).child("■ headers"))
        .child(div().text_color(hsla(0.52, 0.9, 0.55, 1.0)).child("■ selected block"))
        .child(div().text_color(hsla(0.14, 1.0, 0.55, 1.0)).child("■ hovered symbol"));

    v_flex()
        .size_full()
        .bg(theme.background)
        .border_t_1()
        .border_color(theme.border)
        .child(header)
        .child(list)
        .child(status(this, hover_span, &theme))
        .into_any_element()
}

/// Bit-level description of the hovered byte and the symbol under it.
fn status(this: &Visco, hover_span: Option<usize>, theme: &gpui_kit::component::Theme) -> impl IntoElement {
    let mut line = h_flex()
        .px_3()
        .py_1()
        .gap_3()
        .border_t_1()
        .border_color(theme.border)
        .text_xs()
        .font_family(theme.mono_font_family.clone())
        .min_h(px(24.));
    let (Some(trace), Some(b)) = (this.trace(), this.hover_byte) else {
        return line.child(div().text_color(theme.muted_foreground).child("Hover a byte to see the bits and the symbol they encode."));
    };
    let bs = &trace.bitstream;
    let Some(&v) = bs.bytes.get(b) else { return line };
    let span = hover_span.map(|s| &bs.spans[s]);
    let mut bits = h_flex();
    for i in 0..8 {
        let bit = b as u64 * 8 + i;
        let on = (v >> (7 - i)) & 1;
        let in_span = span.is_some_and(|s| s.contains(bit));
        bits = bits.child(
            div()
                .px(px(1.5))
                .when(in_span, |d| d.bg(hsla(0.14, 1.0, 0.55, 0.45)))
                .child(on.to_string()),
        );
    }
    line = line.child(format!("{b:08X}")).child(format!("{v:02X}")).child(bits);
    if let Some(seg) = segment_ix(bs, b) {
        line = line.child(div().text_color(theme.muted_foreground).child(bs.segments[seg].name.clone()));
    }
    if let Some(s) = span {
        line = line
            .child(div().font_semibold().child(s.kind.describe()))
            .child(div().text_color(theme.muted_foreground).child(format!("bits {}+{}", s.start, s.len)));
        if let (Some(id), Some(blocks)) = (s.block_id(), trace.blocks.as_ref()) {
            let _ = blocks;
            line = line.child(div().text_color(theme.muted_foreground).child(format!("block #{id}")));
        }
    }
    line
}
