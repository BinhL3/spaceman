use gpui::{AnyElement, Div, FontWeight, IntoElement, Rgba, div, prelude::*, px, rgb};

const CANVAS: u32 = 0x111111;
const SURFACE: u32 = 0x171717;
const SURFACE_HOVER: u32 = 0x1d1d1d;
const LINE: u32 = 0x292929;
const ACCENT: u32 = 0x3b5cf6;
const SUCCESS: u32 = 0x69d89f;
const SUCCESS_SURFACE: u32 = 0x183026;
const TEXT: u32 = 0xeeeeee;
const TEXT_SOFT: u32 = 0xb8b8b8;
const MUTED: u32 = 0x858585;

pub(crate) fn window_frame() -> Div {
    div().size_full().bg(rgb(CANVAS)).text_color(rgb(TEXT))
}

pub(crate) fn settings_copy(title: impl IntoElement, description: impl IntoElement) -> Div {
    div()
        .min_w(px(0.0))
        .flex_1()
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .text_size(px(13.0))
                .font_weight(FontWeight::SEMIBOLD)
                .child(title),
        )
        .child(
            div()
                .text_size(px(11.0))
                .text_color(rgb(MUTED))
                .child(description),
        )
}

pub(crate) fn compact_button(label: impl IntoElement) -> Div {
    div()
        .h(px(30.0))
        .px_3()
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded_sm()
        .cursor_pointer()
        .text_size(px(12.0))
        .text_color(rgb(TEXT_SOFT))
        .hover(|button| button.bg(rgb(SURFACE_HOVER)))
        .child(label)
}

pub(crate) fn primary_button(label: impl IntoElement) -> Div {
    compact_button(label)
        .rounded(px(6.0))
        .bg(rgb(ACCENT))
        .text_color(rgb(0xffffff))
        .hover(|button| button.bg(rgb(0x4a69f7)))
}

pub(crate) fn success_badge(label: impl IntoElement) -> Div {
    div()
        .h(px(26.0))
        .px_3()
        .flex_none()
        .flex()
        .items_center()
        .gap_1()
        .rounded_full()
        .bg(rgb(SUCCESS_SURFACE))
        .text_size(px(11.0))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(rgb(SUCCESS))
        .child("✓")
        .child(label)
}

pub(crate) fn toggle(position: f32) -> AnyElement {
    let color_position = position.clamp(0.0, 1.0);
    div()
        .w(px(24.0))
        .h(px(16.0))
        .p(px(2.0))
        .flex_none()
        .flex()
        .items_center()
        .rounded(px(4.0))
        .bg(mix_color(rgb(0x3a3a3a), rgb(ACCENT), color_position))
        .child(
            div()
                .ml(px(8.0 * position.clamp(-0.04, 1.04)))
                .size(px(12.0))
                .rounded(px(2.0))
                .bg(mix_color(rgb(0xc8c8c8), rgb(0xfafafa), color_position)),
        )
        .into_any_element()
}

fn mix_color(from: Rgba, to: Rgba, position: f32) -> Rgba {
    let position = position.clamp(0.0, 1.0);
    Rgba {
        r: from.r + (to.r - from.r) * position,
        g: from.g + (to.g - from.g) * position,
        b: from.b + (to.b - from.b) * position,
        a: from.a + (to.a - from.a) * position,
    }
}

pub(crate) fn settings_row(
    title: impl IntoElement,
    description: impl IntoElement,
    control: impl IntoElement,
) -> Div {
    div()
        .w_full()
        .min_h(px(72.0))
        .px_4()
        .py_3()
        .flex()
        .items_center()
        .justify_between()
        .gap_4()
        .border_b_1()
        .border_color(rgb(LINE))
        .child(settings_copy(title, description))
        .child(control)
}

pub(crate) fn settings_panel() -> Div {
    div()
        .w_full()
        .rounded(px(10.0))
        .border_1()
        .border_color(rgb(LINE))
        .bg(rgb(SURFACE))
        .overflow_hidden()
}
