//! The stack of open overlays. Escape closes only the topmost; while a modal is open, every top-level element of the
//! page except those holding that modal and the overlays above it is `inert`. `Stack`, `InertBook` and `tabbable`
//! are pure; `open_layer`, `close_layer`, `layer` and `tabbables` apply them to the page.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LayerId(u64);

#[derive(Debug)]
pub struct Entry<T> {
    pub id: LayerId,
    pub modal: bool,
    pub item: T,
}

/// Open overlays, the most recently opened on top.
#[derive(Debug)]
pub struct Stack<T> {
    entries: Vec<Entry<T>>,
    next: u64,
}

impl<T> Default for Stack<T> {
    fn default() -> Self {
        Stack {
            entries: Vec::new(),
            next: 0,
        }
    }
}

impl<T> Stack<T> {
    pub fn push(&mut self, modal: bool, item: T) -> LayerId {
        let id = LayerId(self.next);
        self.next += 1;
        self.entries.push(Entry { id, modal, item });
        id
    }

    pub fn remove(&mut self, id: LayerId) -> Option<T> {
        let at = self.entries.iter().position(|e| e.id == id)?;
        Some(self.entries.remove(at).item)
    }

    pub fn top(&self) -> Option<&T> {
        self.entries.last().map(|e| &e.item)
    }

    pub fn get_mut(&mut self, id: LayerId) -> Option<&mut T> {
        self.entries
            .iter_mut()
            .find(|e| e.id == id)
            .map(|e| &mut e.item)
    }

    /// The topmost modal and every overlay opened above it, or `None` while no modal is open and the whole page is live.
    pub fn live(&self) -> Option<&[Entry<T>]> {
        let at = self.entries.iter().rposition(|e| e.modal)?;
        Some(&self.entries[at..])
    }
}

/// What the page's top-level elements were before modals made them inert, so they get back exactly that.
#[derive(Debug)]
pub struct InertBook<K> {
    originals: Vec<(K, bool)>,
}

impl<K> Default for InertBook<K> {
    fn default() -> Self {
        InertBook {
            originals: Vec::new(),
        }
    }
}

impl<K: PartialEq + Clone> InertBook<K> {
    /// Given each top-level element as (key, inert now, live), and whether a modal is open, the elements whose inert
    /// state must change, and to what. Only the closing of the last modal restores everything.
    pub fn update(&mut self, children: &[(K, bool, bool)], modal_open: bool) -> Vec<(K, bool)> {
        if !modal_open {
            let current = |k: &K| children.iter().find(|c| &c.0 == k).map(|c| c.1);
            return self
                .originals
                .drain(..)
                .filter(|(k, orig)| current(k).is_some_and(|now| now != *orig))
                .collect();
        }
        self.originals
            .retain(|(k, _)| children.iter().any(|c| &c.0 == k));
        let mut changes = Vec::new();
        for (key, now, live) in children {
            let recorded = self.originals.iter().find(|o| &o.0 == key).map(|o| o.1);
            let want = if *live {
                recorded.unwrap_or(*now)
            } else {
                true
            };
            if want != *now {
                if recorded.is_none() {
                    self.originals.push((key.clone(), *now));
                }
                changes.push((key.clone(), want));
            }
        }
        changes
    }
}

/// What decides whether an element Tab could land on really takes focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Candidate {
    pub disabled: bool,
    pub hidden_input: bool,
    pub tab_index: i32,
    /// It or an ancestor is `hidden` or `inert`.
    pub under_hidden: bool,
    /// It has a box: not `display: none`, nor inside one.
    pub rendered: bool,
    /// Not `visibility: hidden` or `collapse`.
    pub visible: bool,
}

pub fn tabbable(c: &Candidate) -> bool {
    !c.disabled && !c.hidden_input && c.tab_index >= 0 && !c.under_hidden && c.rendered && c.visible
}

pub use page::*;

mod page {
    use std::cell::{Cell, RefCell};

    use leptos::ev;
    use leptos::prelude::*;
    use wasm_bindgen::JsCast;

    use super::*;

    struct Layer {
        close: Callback<()>,
        root: Option<web_sys::Element>,
    }

    thread_local! {
        static STACK: RefCell<Stack<Layer>> = RefCell::new(Stack::default());
        static INERT: RefCell<InertBook<web_sys::Element>> = RefCell::new(InertBook::default());
        static LISTENING: Cell<bool> = const { Cell::new(false) };
    }

    fn listen_for_escape() {
        if LISTENING.replace(true) {
            return;
        }
        // One listener for the page's lifetime: it closes only the topmost overlay.
        let _ = window_event_listener(ev::keydown, |ev| {
            if ev.key() != "Escape" {
                return;
            }
            let close = STACK.with_borrow(|s| s.top().map(|l| l.close));
            if let Some(close) = close {
                ev.prevent_default();
                close.run(());
            }
        });
    }

    fn apply_inert() {
        let Some(body) = document().body() else {
            return;
        };
        let (open, live): (bool, Vec<web_sys::Element>) = STACK.with_borrow(|s| match s.live() {
            None => (false, Vec::new()),
            Some(live) => (
                true,
                live.iter().filter_map(|e| e.item.root.clone()).collect(),
            ),
        });
        let list = body.children();
        let children: Vec<_> = (0..list.length())
            .filter_map(|i| list.item(i))
            .map(|e| {
                let holds = live.iter().any(|r| e.contains(Some(r)));
                let now = e.has_attribute("inert");
                (e, now, holds)
            })
            .collect();
        for (e, inert) in INERT.with_borrow_mut(|b| b.update(&children, open)) {
            let _ = if inert {
                e.set_attribute("inert", "")
            } else {
                e.remove_attribute("inert")
            };
        }
    }

    /// Puts an overlay on the stack: Escape runs `close` while it is topmost. Its root element, once known through
    /// `set_root`, stays live above a modal.
    pub fn open_layer(close: Callback<()>, modal: bool) -> LayerId {
        listen_for_escape();
        STACK.with_borrow_mut(|s| s.push(modal, Layer { close, root: None }))
    }

    pub fn set_root(id: LayerId, root: web_sys::Element) {
        STACK.with_borrow_mut(|s| {
            if let Some(l) = s.get_mut(id) {
                l.root = Some(root);
            }
        });
        apply_inert();
    }

    pub fn close_layer(id: LayerId) {
        STACK.with_borrow_mut(|s| s.remove(id));
        apply_inert();
    }

    /// An overlay's layer for as long as the current owner lives, rooted at `root` once it renders.
    pub fn layer<E>(close: Callback<()>, modal: bool, root: NodeRef<E>) -> LayerId
    where
        E: leptos::html::ElementType + 'static,
        E::Output: JsCast + Clone + 'static,
        NodeRef<E>: Get<Value = Option<E::Output>>,
    {
        let id = open_layer(close, modal);
        Effect::new(move || {
            if let Some(el) = root.get() {
                set_root(id, el.unchecked_into());
            }
        });
        on_cleanup(move || close_layer(id));
        id
    }

    const CANDIDATES: &str = "a[href], area[href], button, input, select, textarea, iframe, summary, \
                              [contenteditable]:not([contenteditable='false']), [tabindex]";

    fn candidate(e: &web_sys::HtmlElement) -> Candidate {
        let visibility = window()
            .get_computed_style(e)
            .ok()
            .flatten()
            .and_then(|s| s.get_property_value("visibility").ok());
        Candidate {
            disabled: e.matches(":disabled").unwrap_or(false),
            hidden_input: e.matches("input[type=hidden]").unwrap_or(false),
            tab_index: e.tab_index(),
            under_hidden: e.closest("[hidden], [inert]").ok().flatten().is_some(),
            rendered: e.get_client_rects().length() > 0,
            visible: !matches!(visibility.as_deref(), Some("hidden" | "collapse")),
        }
    }

    /// The elements within `root` that Tab really reaches, in document order.
    pub fn tabbables(root: &web_sys::Element) -> Vec<web_sys::HtmlElement> {
        let Ok(list) = root.query_selector_all(CANDIDATES) else {
            return Vec::new();
        };
        (0..list.length())
            .filter_map(|i| list.item(i))
            .filter_map(|n| n.dyn_into::<web_sys::HtmlElement>().ok())
            .filter(|e| tabbable(&candidate(e)))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escape_reaches_only_the_topmost_overlay() {
        let mut s = Stack::default();
        let drawer = s.push(true, "drawer");
        let menu = s.push(false, "menu");
        let tip = s.push(false, "tip");
        assert_eq!(s.top(), Some(&"tip"));
        assert_eq!(s.remove(menu), Some("menu"));
        assert_eq!(s.top(), Some(&"tip"));
        assert_eq!(s.remove(tip), Some("tip"));
        assert_eq!(s.top(), Some(&"drawer"));
        assert_eq!(s.remove(tip), None);
        s.remove(drawer);
        assert_eq!(s.top(), None);
    }

    #[test]
    fn the_topmost_modal_and_what_opened_above_it_stay_live() {
        let mut s = Stack::default();
        s.push(false, "tip");
        assert!(s.live().is_none());
        s.push(true, "first");
        let second = s.push(true, "second");
        s.push(false, "menu");
        let live: Vec<_> = s.live().unwrap().iter().map(|e| e.item).collect();
        assert_eq!(live, ["second", "menu"]);
        s.remove(second);
        let live: Vec<_> = s.live().unwrap().iter().map(|e| e.item).collect();
        assert_eq!(live, ["first", "menu"]);
    }

    #[test]
    fn overlapping_modals_restore_the_page_only_when_the_last_closes() {
        let mut book = InertBook::default();
        // The page, an element that was inert already, and the first drawer.
        let changes = book.update(
            &[
                ("app", false, false),
                ("aside", true, false),
                ("d1", false, true),
            ],
            true,
        );
        assert_eq!(changes, [("app", true)]);
        // A second drawer over the first.
        let changes = book.update(
            &[
                ("app", true, false),
                ("aside", true, false),
                ("d1", false, false),
                ("d2", false, true),
            ],
            true,
        );
        assert_eq!(changes, [("d1", true)]);
        // The second closes: the first wakes, the page stays inert.
        let changes = book.update(
            &[
                ("app", true, false),
                ("aside", true, false),
                ("d1", true, true),
            ],
            true,
        );
        assert_eq!(changes, [("d1", false)]);
        // The last closes: only what the drawers made inert wakes.
        let changes = book.update(&[("app", true, false), ("aside", true, false)], false);
        assert_eq!(changes, [("app", false)]);
        assert_eq!(book.update(&[("app", false, false)], false), []);
    }

    #[test]
    fn tab_skips_what_cannot_take_focus() {
        let plain = Candidate {
            disabled: false,
            hidden_input: false,
            tab_index: 0,
            under_hidden: false,
            rendered: true,
            visible: true,
        };
        assert!(tabbable(&plain));
        for skipped in [
            Candidate {
                disabled: true,
                ..plain
            },
            Candidate {
                hidden_input: true,
                ..plain
            },
            Candidate {
                tab_index: -1,
                ..plain
            },
            Candidate {
                under_hidden: true,
                ..plain
            },
            Candidate {
                rendered: false,
                ..plain
            },
            Candidate {
                visible: false,
                ..plain
            },
        ] {
            assert!(!tabbable(&skipped), "{skipped:?}");
        }
    }
}
