//! The plan review's pure logic, whatever the plan: counts of its items by kind, a kind's name in the URL, and setting
//! one choice for many items at once. `parts::plan` shows it; `repair_model` holds what is particular to repairs.

use std::collections::BTreeMap;

use super::status::Status;

/// How many items of each kind a plan holds, in the vocabulary's order, leaving out kinds it has none of.
pub fn kind_counts<K: Status>(kinds: impl IntoIterator<Item = K>) -> Vec<(K, usize)> {
    let mut counts = vec![0; K::ALL.len()];
    for kind in kinds {
        counts[kind.rank()] += 1;
    }
    K::ALL
        .iter()
        .zip(counts)
        .filter(|(_, n)| *n > 0)
        .map(|(k, n)| (*k, n))
        .collect()
}

/// A kind as the URL's `kind` names it: its label, lowercase, with hyphens for spaces.
pub fn kind_slug<K: Status>(kind: K) -> String {
    kind.look().label.to_lowercase().replace(' ', "-")
}

/// The kind the URL's `kind` names, if it names one.
pub fn kind_from_slug<K: Status>(slug: &str) -> Option<K> {
    K::ALL.iter().copied().find(|k| kind_slug(*k) == slug)
}

/// What setting one choice for many items did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BulkOutcome {
    /// The items it was set for.
    pub asked: usize,
    /// Of those, the items that offer it.
    pub offered: usize,
    /// Of those, the items whose choice it changed: the rest had it already.
    pub changed: usize,
}

impl BulkOutcome {
    /// What to tell you, naming the choice as `label`.
    pub fn message(&self, label: &str) -> String {
        let BulkOutcome {
            asked,
            offered,
            changed,
        } = *self;
        let items = |n: usize| if n == 1 { "item" } else { "items" };
        let mut said = match changed {
            0 => format!("No item changed to “{label}”"),
            n => format!("Changed {n} {} to “{label}”", items(n)),
        };
        if offered > changed {
            said.push_str(&format!("; {} had it already", offered - changed));
        }
        if offered < asked {
            let left = asked - offered;
            said.push_str(&format!(
                "; {left} {} {} offer it, so {} unchanged",
                items(left),
                if left == 1 { "doesn't" } else { "don't" },
                if left == 1 { "it's" } else { "they're" },
            ));
        }
        said.push('.');
        said
    }
}

/// Sets `choice` for every item of `items` that offers it, in `picked` by key, where an item without a pick takes
/// `current`. `items` are each item's key, the choice it takes now and the choices it offers.
pub fn apply_choice<'a, C: Copy + Eq + 'a>(
    items: impl IntoIterator<Item = (&'a str, C, &'a [C])>,
    choice: C,
    picked: &mut BTreeMap<String, C>,
) -> BulkOutcome {
    let mut outcome = BulkOutcome::default();
    for (key, current, offers) in items {
        outcome.asked += 1;
        if !offers.contains(&choice) {
            continue;
        }
        outcome.offered += 1;
        let now = picked.get(key).copied().unwrap_or(current);
        if now != choice {
            outcome.changed += 1;
        }
        picked.insert(key.to_string(), choice);
    }
    outcome
}

/// The choices every one of `offers` offers, in the first's order: what "apply to all" can set for a kind.
pub fn offered_by_all<'a, C: Copy + Eq + 'a>(offers: impl IntoIterator<Item = &'a [C]>) -> Vec<C> {
    let mut offers = offers.into_iter();
    let Some(first) = offers.next() else {
        return Vec::new();
    };
    let mut common = first.to_vec();
    for o in offers {
        common.retain(|c| o.contains(c));
    }
    common
}

/// The choices any of `offers` offers, in the order first met: what can be set for rows of mixed offers.
pub fn offered_by_any<'a, C: Copy + Eq + 'a>(offers: impl IntoIterator<Item = &'a [C]>) -> Vec<C> {
    let mut any = Vec::new();
    for o in offers {
        for c in o {
            if !any.contains(c) {
                any.push(*c);
            }
        }
    }
    any
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parts::status::PlanKind;

    #[test]
    fn counts_follow_the_vocabularys_order_and_leave_out_kinds_absent() {
        let kinds = [PlanKind::SetAside, PlanKind::Rename, PlanKind::SetAside];
        assert_eq!(
            kind_counts(kinds),
            [(PlanKind::Rename, 1), (PlanKind::SetAside, 2)]
        );
    }

    #[test]
    fn every_kind_round_trips_through_the_url() {
        for kind in PlanKind::ALL {
            let slug = kind_slug(*kind);
            assert!(!slug.contains(' '), "{slug}");
            assert_eq!(kind_from_slug::<PlanKind>(&slug), Some(*kind));
        }
        assert_eq!(kind_from_slug::<PlanKind>("nonsense"), None);
    }

    #[test]
    fn a_choice_is_set_only_where_offered_and_counted_only_where_it_changed() {
        let mut picked = BTreeMap::from([("b".to_string(), 2)]);
        let items: [(&str, u8, &[u8]); 3] = [("a", 1, &[1, 2]), ("b", 1, &[1, 2]), ("c", 1, &[1])];
        let outcome = apply_choice(items, 2, &mut picked);
        assert_eq!(
            outcome,
            BulkOutcome {
                asked: 3,
                offered: 2,
                changed: 1
            }
        );
        assert_eq!(picked, BTreeMap::from([("a".into(), 2), ("b".into(), 2)]));
    }

    #[test]
    fn the_message_says_how_many_changed_and_why_the_rest_didnt() {
        let all = BulkOutcome {
            asked: 3,
            offered: 3,
            changed: 3,
        };
        assert_eq!(all.message("leave"), "Changed 3 items to “leave”.");
        let some = BulkOutcome {
            asked: 5,
            offered: 3,
            changed: 2,
        };
        assert_eq!(
            some.message("leave"),
            "Changed 2 items to “leave”; 1 had it already; 2 items don't offer it, so they're unchanged."
        );
        let none = BulkOutcome {
            asked: 1,
            offered: 0,
            changed: 0,
        };
        assert_eq!(
            none.message("apply"),
            "No item changed to “apply”; 1 item doesn't offer it, so it's unchanged."
        );
    }

    #[test]
    fn all_offers_what_every_item_does_and_any_what_one_does() {
        let offers: [&[u8]; 2] = [&[1, 2, 3], &[3, 2]];
        assert_eq!(offered_by_all(offers), [2, 3]);
        assert_eq!(offered_by_any(offers), [1, 2, 3]);
        assert!(offered_by_all::<u8>([]).is_empty());
    }
}
