//! The pure logic behind `parts::list`: which rows a virtualized list renders, where the keyboard cursor moves,
//! which rows are selected, which toolbar buttons fit, and the filtered and sorted order of the rows.

use std::cmp::Ordering;
use std::collections::BTreeSet;
use std::ops::{Range, RangeInclusive};

/// The rows to render for a list scrolled to `scroll_top` with a viewport `viewport` tall, of `count` rows each
/// `row_height` tall, with `overscan` extra rows either side.
pub fn window(
    scroll_top: f64,
    viewport: f64,
    row_height: f64,
    count: usize,
    overscan: usize,
) -> Range<usize> {
    if count == 0 || row_height <= 0.0 {
        return 0..0;
    }
    let top = scroll_top
        .min(count as f64 * row_height - viewport)
        .max(0.0);
    let first = (top / row_height).floor() as usize;
    let last = ((top + viewport) / row_height).ceil() as usize;
    first.saturating_sub(overscan)..last.saturating_add(overscan).min(count)
}

/// A key that moves a list's cursor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Up,
    Down,
    PageUp,
    PageDown,
    Home,
    End,
}

impl Step {
    /// The step a key's `KeyboardEvent.key` names, if any.
    pub fn of(key: &str) -> Option<Step> {
        Some(match key {
            "ArrowUp" => Step::Up,
            "ArrowDown" => Step::Down,
            "PageUp" => Step::PageUp,
            "PageDown" => Step::PageDown,
            "Home" => Step::Home,
            "End" => Step::End,
            _ => return None,
        })
    }
}

/// Where the cursor goes from `at` on `step` among `count` rows, `page` rows to a page. With no cursor yet, any step
/// lands on the first row.
pub fn step(at: Option<usize>, step: Step, count: usize, page: usize) -> Option<usize> {
    let last = count.checked_sub(1)?;
    let Some(at) = at else {
        return Some(0);
    };
    if at > last {
        return Some(last);
    }
    Some(match step {
        Step::Up => at.saturating_sub(1),
        Step::Down => (at + 1).min(last),
        Step::PageUp => at.saturating_sub(page.max(1)),
        Step::PageDown => (at + page.max(1)).min(last),
        Step::Home => 0,
        Step::End => last,
    })
}

/// The scroll position that brings row `index` fully into a viewport `viewport` tall scrolled to `scroll_top`, or
/// `None` when it is already in view.
pub fn reveal(index: usize, row_height: f64, scroll_top: f64, viewport: f64) -> Option<f64> {
    let top = index as f64 * row_height;
    if top < scroll_top {
        Some(top)
    } else if top + row_height > scroll_top + viewport {
        Some(top + row_height - viewport)
    } else {
        None
    }
}

/// The rows a list's checkboxes select, by key: either the rows chosen one by one, or every row matching the filter
/// except those unchecked since.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Selection {
    /// Every row matching the filter, rather than only `keys`.
    pub matching: bool,
    /// The rows chosen, or with `matching`, the rows left out.
    pub keys: BTreeSet<String>,
}

/// The state of a list's select-all checkbox.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Checked {
    None,
    Some,
    All,
}

impl Selection {
    pub fn is_empty(&self) -> bool {
        !self.matching && self.keys.is_empty()
    }

    pub fn contains(&self, key: &str) -> bool {
        self.matching != self.keys.contains(key)
    }

    /// Checks `key` if unchecked, and unchecks it if checked.
    pub fn toggle(&mut self, key: &str) {
        if !self.keys.remove(key) {
            self.keys.insert(key.to_string());
        }
    }

    /// Checks every key of `keys`, or unchecks them all.
    pub fn set<'a>(&mut self, keys: impl IntoIterator<Item = &'a str>, on: bool) {
        // With `matching`, `keys` holds the rows left out, so checking a row removes it.
        let listed = on != self.matching;
        for key in keys {
            if listed {
                self.keys.insert(key.to_string());
            } else {
                self.keys.remove(key);
            }
        }
    }

    /// Every row matching the filter.
    pub fn select_matching(&mut self) {
        self.matching = true;
        self.keys.clear();
    }

    /// A click on the select-all checkbox, when `listed` of the `shown` rows are in `keys`: it checks every matching
    /// row, unless every one already is; then it clears.
    pub fn select_all_or_none(&mut self, shown: usize, listed: usize) {
        if self.checked(shown, listed) == Checked::All {
            self.clear();
        } else {
            self.select_matching();
        }
    }

    pub fn clear(&mut self) {
        self.matching = false;
        self.keys.clear();
    }

    /// After the filter changes: a selection of every matching row no longer means what it did, so it clears; rows
    /// chosen one by one stay chosen, shown or not.
    pub fn refiltered(&mut self) {
        if self.matching {
            self.clear();
        }
    }

    /// How many of the `shown` rows matching the filter are checked, when `listed` of them are in `keys`.
    pub fn shown_count(&self, shown: usize, listed: usize) -> usize {
        if self.matching {
            shown.saturating_sub(listed)
        } else {
            listed
        }
    }

    /// How many rows are checked in all, when `listed` of the `shown` rows are in `keys`: rows chosen one by one count
    /// whether shown or not, and a left-out row that no longer matches doesn't count against the matching rows.
    pub fn count(&self, shown: usize, listed: usize) -> usize {
        if self.matching {
            self.shown_count(shown, listed)
        } else {
            self.keys.len()
        }
    }

    /// The select-all checkbox's state, when `listed` of the `shown` rows are in `keys`.
    pub fn checked(&self, shown: usize, listed: usize) -> Checked {
        match self.shown_count(shown, listed) {
            0 => Checked::None,
            n if n >= shown => Checked::All,
            _ => Checked::Some,
        }
    }
}

/// The rows from `anchor` to `to`, either way round: a Shift-click's range.
pub fn span(anchor: usize, to: usize) -> RangeInclusive<usize> {
    anchor.min(to)..=anchor.max(to)
}

/// The positions of `rows` in the order `compare` puts them, reversed when `descending`. Rows that compare equal
/// keep their order either way.
pub fn sorted<T>(rows: &[T], compare: impl Fn(&T, &T) -> Ordering, descending: bool) -> Vec<u32> {
    let mut order: Vec<u32> = (0..rows.len() as u32).collect();
    order.sort_by(|&a, &b| {
        let ord = compare(&rows[a as usize], &rows[b as usize]);
        if descending { ord.reverse() } else { ord }
    });
    order
}

/// The text a row's search text is matched against: trimmed and lowercased.
pub fn needle(filter: &str) -> String {
    filter.trim().to_lowercase()
}

/// The positions of `order` whose lowercased search text in `search` contains `needle` (see [`needle`]) and that
/// `keep` keeps, in that order.
pub fn matching(
    search: &[String],
    order: &[u32],
    needle: &str,
    keep: impl Fn(usize) -> bool,
) -> Vec<u32> {
    order
        .iter()
        .copied()
        .filter(|&i| {
            let i = i as usize;
            (needle.is_empty() || search[i].contains(needle)) && keep(i)
        })
        .collect()
}

/// How many of a toolbar's buttons, `widths` wide in order with `gap` between them, fit in `room`. When some don't,
/// room is kept for the overflow menu's button, `more` wide, and the rest go into that menu.
pub fn fit(widths: &[f64], gap: f64, room: f64, more: f64) -> usize {
    let total: f64 = widths.iter().sum::<f64>() + gap * widths.len().saturating_sub(1) as f64;
    if total <= room {
        return widths.len();
    }
    let mut used = more;
    widths
        .iter()
        .take_while(|w| {
            used += *w + gap;
            used <= room
        })
        .count()
}

/// How wide a list's column is, in rem.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Width {
    /// Exactly this wide: icons, numbers, times.
    Fixed(f32),
    /// At least `min` wide, sharing the room left over with the other flexible columns by `share`: names and paths.
    Flex { min: f32, share: u8 },
}

/// The CSS grid columns of a list with a checkbox column `check` rem wide (0 for none), its `columns`, and a column
/// of row actions `actions` rem wide (0 for none); and the narrowest the list can be, in rem, before it scrolls
/// sideways.
pub fn template(check: f32, columns: &[Width], actions: f32) -> (String, f32) {
    let check = (check > 0.0).then_some(Width::Fixed(check));
    let actions = (actions > 0.0).then_some(Width::Fixed(actions));
    let all: Vec<Width> = check
        .into_iter()
        .chain(columns.iter().copied())
        .chain(actions)
        .collect();
    let tracks: Vec<String> = all
        .iter()
        .map(|w| match w {
            Width::Fixed(w) => format!("{w}rem"),
            Width::Flex { min, share } => format!("minmax({min}rem, {share}fr)"),
        })
        .collect();
    let min = all
        .iter()
        .map(|w| match w {
            Width::Fixed(w) | Width::Flex { min: w, .. } => *w,
        })
        .sum();
    (tracks.join(" "), min)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_window_covers_the_viewport_and_its_overscan() {
        // Rows 10 to 19 are in view; three more either side.
        assert_eq!(window(400.0, 400.0, 40.0, 1000, 3), 7..23);
        // A row partly in view is rendered.
        assert_eq!(window(420.0, 400.0, 40.0, 1000, 0), 10..21);
    }

    #[test]
    fn a_window_stays_within_the_rows() {
        assert_eq!(window(0.0, 400.0, 40.0, 1000, 5), 0..15);
        assert_eq!(window(39_600.0, 400.0, 40.0, 1000, 5), 985..1000);
        assert_eq!(window(0.0, 400.0, 40.0, 4, 5), 0..4);
        assert_eq!(window(0.0, 400.0, 40.0, 0, 5), 0..0);
        // Scrolled past the end, as after a filter shortens the list.
        assert_eq!(window(80_000.0, 400.0, 40.0, 30, 5), 15..30);
    }

    #[test]
    fn arrow_keys_move_the_cursor_and_stop_at_the_ends() {
        assert_eq!(Step::of("ArrowDown"), Some(Step::Down));
        assert_eq!(Step::of("x"), None);
        assert_eq!(step(None, Step::Down, 5, 3), Some(0));
        assert_eq!(step(None, Step::Up, 5, 3), Some(0));
        assert_eq!(step(Some(1), Step::Down, 5, 3), Some(2));
        assert_eq!(step(Some(4), Step::Down, 5, 3), Some(4));
        assert_eq!(step(Some(0), Step::Up, 5, 3), Some(0));
        assert_eq!(step(Some(1), Step::PageDown, 5, 3), Some(4));
        assert_eq!(step(Some(3), Step::PageUp, 5, 3), Some(0));
        assert_eq!(step(Some(2), Step::Home, 5, 3), Some(0));
        assert_eq!(step(Some(2), Step::End, 5, 3), Some(4));
        assert_eq!(step(Some(2), Step::Down, 0, 3), None);
        // A cursor left beyond a list that shrank comes back onto it.
        assert_eq!(step(Some(9), Step::Up, 5, 3), Some(4));
    }

    #[test]
    fn revealing_a_row_scrolls_only_as_far_as_needed() {
        // Rows 10 to 19 are in view.
        assert_eq!(reveal(12, 40.0, 400.0, 400.0), None);
        assert_eq!(reveal(9, 40.0, 400.0, 400.0), Some(360.0));
        assert_eq!(reveal(20, 40.0, 400.0, 400.0), Some(440.0));
    }

    #[test]
    fn rows_are_checked_one_by_one_or_in_a_range() {
        let mut s = Selection::default();
        assert!(s.is_empty());
        s.toggle("a");
        assert!(s.contains("a") && !s.contains("b"));
        s.set(["b", "c"], true);
        // Chosen rows count whether the filter shows them or not.
        assert_eq!(s.count(10, 1), 3);
        assert_eq!(s.shown_count(10, 1), 1);
        s.set(["a", "b"], false);
        assert!(!s.contains("a") && s.contains("c"));
        s.toggle("c");
        assert!(s.is_empty());
        assert_eq!(span(7, 3), 3..=7);
    }

    #[test]
    fn selecting_every_matching_row_leaves_out_those_unchecked_since() {
        let mut s = Selection::default();
        s.select_matching();
        assert!(s.contains("anything"));
        assert_eq!(s.count(100_000, 0), 100_000);
        s.toggle("b");
        assert!(!s.contains("b"));
        assert_eq!(s.count(100_000, 1), 99_999);
        // A left-out row that no longer matches doesn't count against the matching rows.
        assert_eq!(s.count(100_000, 0), 100_000);
        s.toggle("b");
        assert_eq!(s.count(100_000, 0), 100_000);
        s.clear();
        assert!(s.is_empty() && !s.contains("b"));
    }

    #[test]
    fn the_select_all_checkbox_shows_none_some_or_all() {
        let mut s = Selection::default();
        assert_eq!(s.checked(5, 0), Checked::None);
        s.set(["a", "b"], true);
        assert_eq!(s.checked(5, 2), Checked::Some);
        assert_eq!(s.checked(2, 2), Checked::All);
        // Chosen rows the filter hides don't check the box.
        assert_eq!(s.checked(5, 0), Checked::None);
        assert_eq!(s.checked(0, 0), Checked::None);
        let mut s = Selection::default();
        s.select_matching();
        assert_eq!(s.checked(5, 0), Checked::All);
        s.toggle("a");
        assert_eq!(s.checked(5, 1), Checked::Some);
    }

    #[test]
    fn the_select_all_checkbox_checks_every_match_unless_all_are_checked() {
        let mut s = Selection::default();
        s.toggle("a");
        // One of three checked: the box checks them all rather than clearing the one.
        s.select_all_or_none(3, 1);
        assert_eq!(s.checked(3, 0), Checked::All);
        s.select_all_or_none(3, 0);
        assert!(s.is_empty());
        s.select_all_or_none(3, 0);
        assert_eq!(s.checked(3, 0), Checked::All);
        // Every row checked one by one: the box clears.
        let mut s = Selection::default();
        s.set(["a", "b"], true);
        s.select_all_or_none(2, 2);
        assert!(s.is_empty());
    }

    #[test]
    fn a_new_filter_clears_a_matching_selection_but_keeps_chosen_rows() {
        let mut s = Selection::default();
        s.toggle("a");
        s.refiltered();
        assert!(s.contains("a"));
        s.select_matching();
        s.refiltered();
        assert!(s.is_empty());
    }

    #[test]
    fn sorting_is_stable_both_ways() {
        let rows = [("b", 1), ("a", 2), ("b", 3), ("c", 4)];
        let by_name = |a: &(&str, i32), b: &(&str, i32)| a.0.cmp(b.0);
        assert_eq!(sorted(&rows, by_name, false), vec![1, 0, 2, 3]);
        assert_eq!(sorted(&rows, by_name, true), vec![3, 0, 2, 1]);
    }

    #[test]
    fn the_filter_matches_search_text_ignoring_case_and_keeps_the_order() {
        let search: Vec<String> = ["river raid (usa)", "pitfall (europe)", "river city (japan)"]
            .map(String::from)
            .to_vec();
        let order = [2, 1, 0];
        assert_eq!(needle("  River "), "river");
        assert_eq!(
            matching(&search, &order, &needle("  River "), |_| true),
            vec![2, 0]
        );
        assert_eq!(matching(&search, &order, "", |_| true), vec![2, 1, 0]);
        assert_eq!(matching(&search, &order, "river", |i| i != 2), vec![0]);
        assert!(matching(&search, &order, "zzz", |_| true).is_empty());
    }

    #[test]
    fn toolbar_buttons_that_dont_fit_go_into_the_overflow_menu() {
        let widths = [100.0, 80.0, 120.0];
        // 100 + 8 + 80 + 8 + 120 = 316.
        assert_eq!(fit(&widths, 8.0, 316.0, 40.0), 3);
        // Two buttons and the menu: 100 + 8 + 80 + 8 + 40 = 236.
        assert_eq!(fit(&widths, 8.0, 315.0, 40.0), 2);
        assert_eq!(fit(&widths, 8.0, 236.0, 40.0), 2);
        assert_eq!(fit(&widths, 8.0, 235.0, 40.0), 1);
        assert_eq!(fit(&widths, 8.0, 50.0, 40.0), 0);
        assert_eq!(fit(&[], 8.0, 0.0, 40.0), 0);
    }

    #[test]
    fn flexible_columns_share_the_room_above_their_minimum() {
        let (columns, min) = template(
            2.5,
            &[
                Width::Flex {
                    min: 12.0,
                    share: 2,
                },
                Width::Fixed(6.0),
                Width::Flex { min: 8.0, share: 1 },
            ],
            5.0,
        );
        assert_eq!(
            columns,
            "2.5rem minmax(12rem, 2fr) 6rem minmax(8rem, 1fr) 5rem"
        );
        assert_eq!(min, 33.5);
        assert_eq!(
            template(0.0, &[Width::Fixed(4.5)], 0.0),
            ("4.5rem".to_string(), 4.5)
        );
    }
}
