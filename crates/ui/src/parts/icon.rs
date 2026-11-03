//! Icons: vendored Lucide glyphs behind `Icon`, which can't be drawn without a tooltip and an accessible name, and
//! `IconButton` for actions shown as an icon alone.

use leptos::prelude::*;

pub use super::lucide::Glyph;
use super::overlay::Tooltip;

/// The bare SVG of `glyph`: named by `label` when given, else hidden from assistive technology. Screens use `Icon`.
#[component]
pub(crate) fn Svg(
    glyph: Glyph,
    #[prop(optional, into)] label: Option<Signal<String>>,
) -> impl IntoView {
    let hidden = label.is_none().then_some("true");
    let role = label.is_some().then_some("img");
    view! {
        <svg
            class="icon"
            viewBox="0 0 24 24"
            fill="none"
            stroke="currentColor"
            stroke-width="2"
            stroke-linecap="round"
            stroke-linejoin="round"
            focusable="false"
            role=role
            aria-hidden=hidden
            aria-label=move || label.map(|l| l.get())
            inner_html=glyph.body()
        ></svg>
    }
}

/// An icon, with `label` as both its tooltip and its accessible name, and a tab stop so the keyboard reaches its
/// tooltip. Inside a link or button, draw the glyph with `Svg` and name the control instead. Colour it by wrapping it in an element with a
/// `tone-*` class; a status's icon comes from `parts::status::StatusIcon`.
#[component]
pub fn Icon(glyph: Glyph, #[prop(into)] label: Signal<String>) -> impl IntoView {
    view! {
        <Tooltip label focusable=true>
            <Svg glyph label />
        </Tooltip>
    }
}

/// A button shown as an icon alone, named by `label` for its tooltip and for assistive technology. Not for
/// destructive actions: those carry a visible label and ask you to confirm.
#[component]
pub fn IconButton(
    glyph: Glyph,
    #[prop(into)] label: Signal<String>,
    #[prop(into)] on_click: Callback<()>,
    #[prop(optional, into)] disabled: MaybeProp<bool>,
) -> impl IntoView {
    view! {
        <Tooltip label>
            <button
                type="button"
                class="icon-button"
                aria-label=move || label.get()
                disabled=move || disabled.get().unwrap_or(false)
                on:click=move |_| on_click.run(())
            >
                <Svg glyph />
            </button>
        </Tooltip>
    }
}

/// An icon with its label shown beside it, such as in a toolbar button: the visible label names it, so the icon is
/// hidden from assistive technology rather than named twice, and needs no tooltip.
#[component]
pub fn LabelledIcon(glyph: Glyph, #[prop(into)] label: Signal<String>) -> impl IntoView {
    view! {
        <span class="labelled-icon">
            <Svg glyph />
            <span>{move || label.get()}</span>
        </span>
    }
}
