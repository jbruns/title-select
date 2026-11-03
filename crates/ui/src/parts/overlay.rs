//! Overlays hosted in a `<Portal>` on the page's body, so no table cell or scrolling pane clips them: `Tooltip`,
//! `Menu` with its `MenuItem`s, and `Drawer`. `place` positions them against what opened them.

use leptos::ev::KeyboardEvent;
use leptos::html;
use leptos::portal::Portal;
use leptos::prelude::*;
use send_wrapper::SendWrapper;
use wasm_bindgen::JsCast;

use super::icon::{Glyph, IconButton, Svg};

/// A box in viewport coordinates, in CSS pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub left: f64,
    pub top: f64,
    pub right: f64,
    pub bottom: f64,
}

/// Which edge of the overlay lines up with what opened it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Align {
    Start,
    Center,
    End,
}

/// Where an overlay goes: under or above its anchor, lined up with one of its edges, inside the viewport.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Placement {
    pub align: Align,
    pub above: bool,
    anchor: Rect,
    viewport: (f64, f64),
}

const GAP: f64 = 6.0;

/// Places an overlay that needs `room` pixels of height beside `anchor` in a viewport of `width` × `height`. It
/// opens below unless only above has the room; a centred overlay near either side of the viewport lines up with that
/// side's edge of its anchor instead, and one that opens from the start does so near the right side.
pub fn place(anchor: Rect, width: f64, height: f64, room: f64, centred: bool) -> Placement {
    let above = anchor.bottom + GAP + room > height && anchor.top - GAP - room >= 0.0;
    let middle = (anchor.left + anchor.right) / 2.0;
    let align = if centred && middle < width / 4.0 {
        Align::Start
    } else if middle > width * 3.0 / 4.0 {
        Align::End
    } else if centred {
        Align::Center
    } else {
        Align::Start
    };
    Placement {
        align,
        above,
        anchor,
        viewport: (width, height),
    }
}

impl Placement {
    /// The overlay's fixed position, as an inline style.
    pub fn style(&self) -> String {
        let (width, height) = self.viewport;
        let a = self.anchor;
        let vertical = if self.above {
            format!("bottom:{}px", height - a.top + GAP)
        } else {
            format!("top:{}px", a.bottom + GAP)
        };
        let horizontal = match self.align {
            Align::Start => format!("left:{}px", a.left),
            Align::End => format!("right:{}px", width - a.right),
            Align::Center => format!(
                "left:{}px;transform:translateX(-50%)",
                (a.left + a.right) / 2.0
            ),
        };
        format!("position:fixed;{vertical};{horizontal}")
    }
}

fn placement_of(element: &web_sys::Element, room: f64, centred: bool) -> Placement {
    let r = element.get_bounding_client_rect();
    let window = window();
    let size = |v: Result<wasm_bindgen::JsValue, _>| v.ok().and_then(|v| v.as_f64()).unwrap_or(0.0);
    place(
        Rect {
            left: r.left(),
            top: r.top(),
            right: r.right(),
            bottom: r.bottom(),
        },
        size(window.inner_width()),
        size(window.inner_height()),
        room,
        centred,
    )
}

const TOOLTIP_ROOM: f64 = 40.0;
const MENU_ROOM: f64 = 240.0;

/// Shows `label` beside its children while the pointer is over them or focus is in them, until Escape. The label
/// repeats the accessible name the children already carry, so it is hidden from assistive technology.
#[component]
pub fn Tooltip(#[prop(into)] label: Signal<String>, children: Children) -> impl IntoView {
    let anchor = NodeRef::<html::Span>::new();
    let at = RwSignal::new(None::<Placement>);
    let show = move || {
        if let Some(el) = anchor.get_untracked() {
            at.set(Some(placement_of(&el, TOOLTIP_ROOM, true)));
        }
    };
    let hide = move || at.set(None);
    view! {
        <span
            class="tip-anchor"
            node_ref=anchor
            on:mouseenter=move |_| show()
            on:mouseleave=move |_| hide()
            on:focusin=move |_| show()
            on:focusout=move |_| hide()
            on:keydown=move |ev| {
                if ev.key() == "Escape" {
                    hide();
                }
            }
        >
            {children()}
            <Show when=move || at.with(Option::is_some)>
                <Portal>
                    <div
                        class="tooltip"
                        role="tooltip"
                        aria-hidden="true"
                        style=move || at.get().map(|p| p.style())
                    >
                        {move || label.get()}
                    </div>
                </Portal>
            </Show>
        </span>
    }
}

fn menu_items(menu: &web_sys::Element) -> Vec<web_sys::HtmlElement> {
    let Ok(list) = menu.query_selector_all("[role=menuitem]:not([disabled])") else {
        return Vec::new();
    };
    (0..list.length())
        .filter_map(|i| list.item(i))
        .filter_map(|n| n.dyn_into::<web_sys::HtmlElement>().ok())
        .collect()
}

/// Moves focus `by` items through the menu, wrapping at either end.
fn step_focus(menu: &web_sys::Element, by: i32) {
    let items = menu_items(menu);
    if items.is_empty() {
        return;
    }
    let active = document().active_element();
    let at = items
        .iter()
        .position(|i| active.as_ref() == Some(i.unchecked_ref()))
        .map_or(if by > 0 { -1 } else { 0 }, |i| i as i32);
    let next = (at + by).rem_euclid(items.len() as i32) as usize;
    let _ = items[next].focus();
}

/// A button, shown as `glyph` and named `label`, that opens a menu of `MenuItem`s. Choosing an item, clicking
/// outside, or Escape closes it; the arrow keys move between items.
#[component]
pub fn Menu(
    glyph: Glyph,
    #[prop(into)] label: Signal<String>,
    children: ChildrenFn,
) -> impl IntoView {
    let button = NodeRef::<html::Button>::new();
    let list = NodeRef::<html::Div>::new();
    let at = RwSignal::new(None::<Placement>);
    let close = move || {
        at.set(None);
        if let Some(b) = button.get_untracked() {
            let _ = b.focus();
        }
    };
    let toggle = move |_| {
        if at.with_untracked(Option::is_some) {
            close();
        } else if let Some(b) = button.get_untracked() {
            at.set(Some(placement_of(&b, MENU_ROOM, false)));
        }
    };
    Effect::new(move || {
        if let Some(menu) = list.get() {
            step_focus(&menu, 1);
        }
    });
    let children = StoredValue::new(children);
    view! {
        <Tooltip label>
            <button
                type="button"
                class="icon-button"
                node_ref=button
                aria-label=move || label.get()
                aria-haspopup="menu"
                aria-expanded=move || at.with(Option::is_some).to_string()
                on:click=toggle
            >
                <Svg glyph />
            </button>
        </Tooltip>
        <Show when=move || at.with(Option::is_some)>
            <Portal>
                <div class="overlay-scrim" on:click=move |_| close()></div>
                <div
                    class="menu"
                    role="menu"
                    node_ref=list
                    aria-label=move || label.get()
                    style=move || at.get().map(|p| p.style())
                    on:click=move |_| close()
                    on:keydown=move |ev: KeyboardEvent| {
                        let Some(menu) = list.get_untracked() else { return };
                        match ev.key().as_str() {
                            "Escape" | "Tab" => close(),
                            "ArrowDown" => {
                                ev.prevent_default();
                                step_focus(&menu, 1);
                            }
                            "ArrowUp" => {
                                ev.prevent_default();
                                step_focus(&menu, -1);
                            }
                            _ => {}
                        }
                    }
                >
                    {children.with_value(|c| c())}
                </div>
            </Portal>
        </Show>
    }
}

/// One choice in a `Menu`. A destructive choice names what it destroys and asks you to confirm after it's chosen.
#[component]
pub fn MenuItem(
    #[prop(into)] label: Signal<String>,
    #[prop(optional)] glyph: Option<Glyph>,
    #[prop(into)] on_select: Callback<()>,
) -> impl IntoView {
    view! {
        <button type="button" role="menuitem" class="menu-item" on:click=move |_| on_select.run(())>
            {glyph.map(|glyph| view! { <Svg glyph /> })}
            <span>{move || label.get()}</span>
        </button>
    }
}

/// A panel that slides in over the page's side while `open`, titled `title`. Its close button, Escape or clicking
/// beside it closes it, and focus returns to where it was.
#[component]
pub fn Drawer(
    open: RwSignal<bool>,
    #[prop(into)] title: Signal<String>,
    children: ChildrenFn,
) -> impl IntoView {
    let children = StoredValue::new(children);
    view! {
        <Show when=move || open.get()>
            <DrawerPanel open title children=children.get_value() />
        </Show>
    }
}

#[component]
fn DrawerPanel(open: RwSignal<bool>, title: Signal<String>, children: ChildrenFn) -> impl IntoView {
    let panel = NodeRef::<html::Aside>::new();
    let before = SendWrapper::new(
        document()
            .active_element()
            .and_then(|e| e.dyn_into::<web_sys::HtmlElement>().ok()),
    );
    on_cleanup(move || {
        if let Some(e) = before.take() {
            let _ = e.focus();
        }
    });
    Effect::new(move || {
        if let Some(p) = panel.get() {
            let _ = p.focus();
        }
    });
    view! {
        <Portal>
            <div class="overlay-scrim dim" on:click=move |_| open.set(false)></div>
            <aside
                class="drawer"
                role="dialog"
                aria-modal="true"
                aria-label=move || title.get()
                tabindex="-1"
                node_ref=panel
                on:keydown=move |ev: KeyboardEvent| {
                    if ev.key() == "Escape" {
                        open.set(false);
                    }
                }
            >
                <header class="drawer-head">
                    <h2>{move || title.get()}</h2>
                    <IconButton glyph=Glyph::X label="Close" on_click=move |_| open.set(false) />
                </header>
                <div class="drawer-body">{children()}</div>
            </aside>
        </Portal>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const W: f64 = 1200.0;
    const H: f64 = 800.0;

    fn at(left: f64, top: f64) -> Rect {
        Rect {
            left,
            top,
            right: left + 20.0,
            bottom: top + 20.0,
        }
    }

    #[test]
    fn a_tooltip_centres_under_its_anchor() {
        let p = place(at(590.0, 100.0), W, H, TOOLTIP_ROOM, true);
        assert_eq!((p.align, p.above), (Align::Center, false));
        assert_eq!(
            p.style(),
            "position:fixed;top:126px;left:600px;transform:translateX(-50%)"
        );
    }

    #[test]
    fn an_overlay_near_a_side_lines_up_with_that_side() {
        assert_eq!(
            place(at(10.0, 100.0), W, H, TOOLTIP_ROOM, true).align,
            Align::Start
        );
        let p = place(at(1170.0, 100.0), W, H, TOOLTIP_ROOM, true);
        assert_eq!(p.align, Align::End);
        assert_eq!(p.style(), "position:fixed;top:126px;right:10px");
        assert_eq!(
            place(at(1170.0, 100.0), W, H, MENU_ROOM, false).align,
            Align::End
        );
        assert_eq!(
            place(at(600.0, 100.0), W, H, MENU_ROOM, false).align,
            Align::Start
        );
    }

    #[test]
    fn an_overlay_opens_above_only_when_below_lacks_the_room() {
        let p = place(at(600.0, 700.0), W, H, MENU_ROOM, false);
        assert!(p.above);
        assert_eq!(p.style(), "position:fixed;bottom:106px;left:600px");
        assert!(!place(at(600.0, 500.0), W, H, MENU_ROOM, false).above);
        // Too tall for either side: below, where the page can scroll to it.
        assert!(!place(at(600.0, 100.0), W, 300.0, MENU_ROOM, false).above);
    }
}
