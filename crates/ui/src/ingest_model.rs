//! What an Ingest Folder item is to the workbench, and the decisions you make on many items at once: which items a
//! decision applies to, what the strip of counts offers, and the plan request each makes.

use std::collections::BTreeMap;

use title_select_api::additions::NewAddition;
use title_select_api::ingest::{
    IngestAction, IngestFolder, IngestItem, IngestOutcome, IngestPlan, ItemState, PlanIngest,
    Proposal,
};

use crate::model::when;
use crate::parts::icon::Glyph;
use crate::parts::status::{IngestStatus, Status};
use crate::screens::additions::new_addition;

/// Whether a move of the item out of the folder is under way.
pub fn moving(item: &IngestItem) -> bool {
    item.last_move.as_ref().is_some_and(|m| m.under_way)
}

/// When an arriving item in `folder` is next looked at: the follow-up scan the Service has pending, else the folder's
/// own scan cadence, else the next scan, whoever starts it.
pub fn arriving_note(folder: &IngestFolder) -> String {
    let next = match (&folder.next_check_at, folder.scan_every_minutes) {
        (Some(at), _) => format!("next check {}", when(at)),
        (None, Some(1)) => "scanned every minute".into(),
        (None, Some(m)) => format!("scanned every {m} minutes"),
        (None, None) => "at the next scan".into(),
    };
    format!("Assessed once settled · {next}")
}

/// The status the item shows, or none for an item no longer in the folder that no plan moved.
pub fn status(item: &IngestItem) -> Option<IngestStatus> {
    if moving(item) {
        return Some(IngestStatus::Moving);
    }
    Some(match item.state {
        ItemState::Arriving | ItemState::Stale => IngestStatus::Arriving,
        ItemState::AlreadyHeld => IngestStatus::Duplicate,
        ItemState::Unreadable | ItemState::Failed | ItemState::Unsupported => {
            IngestStatus::Unreadable
        }
        ItemState::Dismissed => IngestStatus::Dismissed,
        ItemState::Moved => IngestStatus::Done,
        ItemState::Gone => return None,
        ItemState::Assessed => match &item.assessment.proposal {
            Some(Proposal::SourceSet { .. }) => IngestStatus::MatchesSourceSet,
            Some(Proposal::DatImport) => IngestStatus::DatFile,
            Some(Proposal::Addition { .. }) | None => IngestStatus::NoDatMatch,
        },
    })
}

/// A decision you make on items: one plan for the first three, nothing moved for the last.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Add,
    Import,
    Discard,
    Dismiss,
}

impl Decision {
    pub const ALL: [Decision; 4] = [
        Decision::Add,
        Decision::Import,
        Decision::Discard,
        Decision::Dismiss,
    ];

    /// The plan it makes, if it makes one.
    pub fn action(self) -> Option<IngestAction> {
        match self {
            Decision::Add => Some(IngestAction::Addition),
            Decision::Import => Some(IngestAction::DatImport),
            Decision::Discard => Some(IngestAction::Discard),
            Decision::Dismiss => None,
        }
    }

    pub fn glyph(self) -> Glyph {
        match self {
            Decision::Add => Glyph::CirclePlus,
            Decision::Import => Glyph::Upload,
            Decision::Discard => Glyph::Archive,
            Decision::Dismiss => Glyph::CircleOff,
        }
    }

    /// The toolbar's label, for the checked rows.
    pub fn verb(self) -> &'static str {
        match self {
            Decision::Add => "Add as additions",
            Decision::Import => "Import DATs",
            Decision::Discard => "Discard",
            Decision::Dismiss => "Dismiss",
        }
    }

    /// The label of the decision on `n` items.
    pub fn label(self, n: usize) -> String {
        match (self, n) {
            (Decision::Add, 1) => "Add 1 as an addition".into(),
            (Decision::Add, n) => format!("Add {n} as additions"),
            (Decision::Import, 1) => "Import 1 DAT".into(),
            (Decision::Import, n) => format!("Import {n} DATs"),
            (Decision::Discard, n) => format!("Discard {n}"),
            (Decision::Dismiss, n) => format!("Dismiss {n}"),
        }
    }

    /// The label of the decision on one item, as a row's action.
    pub fn single(self) -> &'static str {
        match self {
            Decision::Add => "Add as an addition",
            Decision::Import => "Import the DAT",
            Decision::Discard => "Discard",
            Decision::Dismiss => "Dismiss until it changes",
        }
    }

    /// What shows while it is under way.
    pub fn pending(self) -> &'static str {
        match self {
            Decision::Add => "Planning additions…",
            Decision::Import => "Planning DAT imports…",
            Decision::Discard => "Planning discards…",
            Decision::Dismiss => "Dismissing…",
        }
    }

    /// Why an item it was asked for was left out.
    pub fn left_out(self, n: usize) -> String {
        let (one, many) = match self {
            Decision::Add => (
                "1 item can't be added as an addition: it matched a DAT, holds more than one file, or has no system yet",
                "items can't be added as additions: they matched a DAT, hold more than one file, or have no system yet",
            ),
            Decision::Import => (
                "1 item isn't a DAT to import, or needs a provider",
                "items aren't DATs to import, or need a provider",
            ),
            Decision::Discard => (
                "1 item can't be discarded yet: it's still arriving, being moved or gone",
                "items can't be discarded yet: they're still arriving, being moved or gone",
            ),
            Decision::Dismiss => (
                "1 item can't be dismissed: it's arriving, dismissed already, being moved or gone",
                "items can't be dismissed: they're arriving, dismissed already, being moved or gone",
            ),
        };
        match n {
            1 => format!("{one}."),
            n => format!("{n} {many}."),
        }
    }
}

/// What you chose beyond the decision: what each item is added as, where you changed it, and the provider to import
/// DATs under when no registered DAT names one.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Choices {
    pub additions: BTreeMap<i64, NewAddition>,
    pub provider: String,
}

impl Choices {
    /// What `item` is added as: what you chose, or its suggested system and name.
    pub fn addition(&self, item: &IngestItem) -> Option<NewAddition> {
        let Some(Proposal::Addition { system }) = &item.assessment.proposal else {
            return None;
        };
        Some(
            self.additions
                .get(&item.id)
                .cloned()
                .unwrap_or_else(|| new_addition(&item.name, system.as_deref())),
        )
    }
}

/// Whether `decision` applies to `item`.
pub fn applies(decision: Decision, item: &IngestItem, choices: &Choices) -> bool {
    if moving(item) {
        return false;
    }
    let state = item.state;
    match decision {
        Decision::Add => {
            state == ItemState::Assessed
                && item.assessment.payloads.len() == 1
                && choices
                    .addition(item)
                    .is_some_and(|a| !a.system.trim().is_empty() && !a.name.trim().is_empty())
        }
        Decision::Import => {
            state == ItemState::Assessed
                && item.assessment.dat.as_ref().is_some_and(|d| {
                    !d.imported && (d.providers.len() == 1 || !choices.provider.trim().is_empty())
                })
        }
        Decision::Discard => matches!(
            state,
            ItemState::Assessed
                | ItemState::AlreadyHeld
                | ItemState::Unreadable
                | ItemState::Unsupported
                | ItemState::Dismissed
        ),
        Decision::Dismiss => matches!(
            state,
            ItemState::Assessed
                | ItemState::AlreadyHeld
                | ItemState::Unreadable
                | ItemState::Unsupported
        ),
    }
}

/// The items a decision applies to, among those it was asked for, and how many it leaves out.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Bulk {
    pub items: Vec<i64>,
    pub left_out: usize,
}

/// Chooses, among `items`, those `decision` applies to.
pub fn choose<'a>(
    decision: Decision,
    items: impl IntoIterator<Item = &'a IngestItem>,
    choices: &Choices,
) -> Bulk {
    let mut bulk = Bulk::default();
    for item in items {
        if applies(decision, item, choices) {
            bulk.items.push(item.id);
        } else {
            bulk.left_out += 1;
        }
    }
    bulk
}

/// The plan request a decision makes for the items `bulk` chose, or none for a dismissal or when it chose none.
pub fn request(
    decision: Decision,
    bulk: &Bulk,
    items: &[IngestItem],
    choices: &Choices,
) -> Option<PlanIngest> {
    let action = decision.action()?;
    if bulk.items.is_empty() {
        return None;
    }
    let provider = choices.provider.trim();
    Some(PlanIngest {
        action,
        items: bulk.items.clone(),
        provider: (action == IngestAction::DatImport && !provider.is_empty())
            .then(|| provider.to_owned()),
        additions: match action {
            IngestAction::Addition => items
                .iter()
                .filter(|i| bulk.items.contains(&i.id))
                .filter_map(|i| choices.addition(i).map(|a| (i.id, a)))
                .collect(),
            _ => BTreeMap::new(),
        },
    })
}

/// The decisions the strip offers for every item of a status.
pub fn offered(status: IngestStatus) -> &'static [Decision] {
    match status {
        IngestStatus::MatchesSourceSet => &[Decision::Dismiss],
        IngestStatus::Duplicate => &[Decision::Discard, Decision::Dismiss],
        IngestStatus::NoDatMatch => &[Decision::Add, Decision::Discard, Decision::Dismiss],
        IngestStatus::DatFile => &[Decision::Import, Decision::Dismiss],
        IngestStatus::Unreadable => &[Decision::Discard, Decision::Dismiss],
        IngestStatus::Dismissed => &[Decision::Discard],
        IngestStatus::Arriving | IngestStatus::Moving | IngestStatus::Done => &[],
    }
}

/// One status on the strip: how many items show it, and each decision you can apply to all of them, with the items
/// it applies to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Count {
    pub status: IngestStatus,
    pub items: usize,
    pub decisions: Vec<(Decision, Bulk)>,
}

/// The strip of counts, one per status present, in the legend's order.
pub fn strip(items: &[IngestItem], choices: &Choices) -> Vec<Count> {
    IngestStatus::ALL
        .iter()
        .filter_map(|&status| {
            let of: Vec<&IngestItem> = items
                .iter()
                .filter(|i| self::status(i) == Some(status))
                .collect();
            if of.is_empty() {
                return None;
            }
            let decisions = offered(status)
                .iter()
                .map(|&d| (d, choose(d, of.iter().copied(), choices)))
                .filter(|(_, bulk)| !bulk.items.is_empty())
                .collect();
            Some(Count {
                status,
                items: of.len(),
                decisions,
            })
        })
        .collect()
}

/// A file's size, in bytes or the largest decimal unit it fills.
pub fn size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["KB", "MB", "GB", "TB"];
    if bytes < 1000 {
        return if bytes == 1 {
            "1 byte".into()
        } else {
            format!("{bytes} bytes")
        };
    }
    let mut value = bytes as f64 / 1000.0;
    let mut unit = 0;
    while value >= 999.95 && unit + 1 < UNITS.len() {
        value /= 1000.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}

/// The `status` URL value that shows only items of a status.
pub fn slug(status: IngestStatus) -> &'static str {
    match status {
        IngestStatus::Arriving => "arriving",
        IngestStatus::MatchesSourceSet => "source-set",
        IngestStatus::Duplicate => "duplicate",
        IngestStatus::NoDatMatch => "no-dat-match",
        IngestStatus::DatFile => "dat",
        IngestStatus::Unreadable => "unreadable",
        IngestStatus::Dismissed => "dismissed",
        IngestStatus::Moving => "moving",
        IngestStatus::Done => "done",
    }
}

/// The status a `status` URL value names.
pub fn parse_slug(value: &str) -> Option<IngestStatus> {
    IngestStatus::ALL
        .iter()
        .copied()
        .find(|s| slug(*s) == value)
}

/// Whether the list shows an item of `status` with `only` chosen: that status alone, or, with none chosen, everything
/// still in the folder.
pub fn shown(only: Option<IngestStatus>, status: IngestStatus) -> bool {
    match only {
        Some(only) => only == status,
        None => status != IngestStatus::Done,
    }
}

/// Where one item of a plan stands: waiting for you to approve the plan, being moved, or how its move ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Outcome {
    Waiting,
    Moving,
    Moved,
    Left,
    Failed,
}

impl Outcome {
    pub fn label(self) -> &'static str {
        match self {
            Outcome::Waiting => "Not yet approved",
            Outcome::Moving => "Moving",
            Outcome::Moved => "Moved",
            Outcome::Left => "Left as it is",
            Outcome::Failed => "Failed",
        }
    }
}

/// Where the item of `plan` whose outcome, if it has one yet, is `ended` stands.
pub fn outcome(plan: &IngestPlan, ended: Option<&IngestOutcome>) -> Outcome {
    match ended {
        Some(o) if o.error.is_some() => Outcome::Failed,
        Some(o) if o.moved => Outcome::Moved,
        Some(_) => Outcome::Left,
        None if plan.approval.is_none() => Outcome::Waiting,
        None if plan.finished_at.is_none() => Outcome::Moving,
        None => Outcome::Left,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use title_select_api::ingest::ItemMove;

    fn item(id: i64, name: &str, state: &str, assessment: serde_json::Value) -> IngestItem {
        serde_json::from_value(serde_json::json!({
            "id": id,
            "name": name,
            "state": state,
            "file": {"size": 4, "mtime_ns": 0},
            "identity": {"dev": 1, "ino": id},
            "first_seen_at": "2026-01-01T00:00:00Z",
            "assessment": assessment,
        }))
        .unwrap()
    }

    fn payload() -> serde_json::Value {
        serde_json::json!({"size": 4, "crc": "c", "md5": "m", "sha1": "s", "sha256": "h"})
    }

    fn homebrew(id: i64, system: Option<&str>) -> IngestItem {
        item(
            id,
            &format!("hack {id}.a26"),
            "assessed",
            serde_json::json!({"payloads": [payload()], "proposal": {"kind": "addition", "system": system}}),
        )
    }

    fn fill(id: i64) -> IngestItem {
        item(
            id,
            "pong.a26",
            "assessed",
            serde_json::json!({"payloads": [payload()], "proposal": {
                "kind": "source_set", "source_set": "Atari", "game": "Pong", "need": "missing"}}),
        )
    }

    fn dat(id: i64, providers: &[&str], imported: bool) -> IngestItem {
        item(
            id,
            "new.dat",
            if imported { "already_held" } else { "assessed" },
            serde_json::json!({"dat": {"name": "Atari", "version": "2", "sha256": "d",
                "providers": providers, "imported": imported},
                "proposal": {"kind": "dat_import"}}),
        )
    }

    fn duplicate(id: i64) -> IngestItem {
        item(id, "pong copy.a26", "already_held", serde_json::json!({}))
    }

    fn being_moved(mut item: IngestItem, under_way: bool) -> IngestItem {
        item.last_move = Some(ItemMove {
            plan: Some(9),
            repair: None,
            job: Some(4),
            under_way,
            finished_at: None,
            error: None,
            note: None,
        });
        item
    }

    #[test]
    fn an_item_shows_what_it_is_and_a_move_under_way_shows_over_its_state() {
        let mut moved = homebrew(6, None);
        moved.state = ItemState::Moved;
        let cases = [
            (fill(1), Some(IngestStatus::MatchesSourceSet)),
            (homebrew(2, None), Some(IngestStatus::NoDatMatch)),
            (dat(3, &[], false), Some(IngestStatus::DatFile)),
            (duplicate(4), Some(IngestStatus::Duplicate)),
            (
                item(5, "x.7z", "unsupported", serde_json::json!({})),
                Some(IngestStatus::Unreadable),
            ),
            (
                item(5, "x.zip", "failed", serde_json::json!({})),
                Some(IngestStatus::Unreadable),
            ),
            (
                item(5, "x.a26", "stale", serde_json::json!({})),
                Some(IngestStatus::Arriving),
            ),
            (
                item(5, "x.a26", "dismissed", serde_json::json!({})),
                Some(IngestStatus::Dismissed),
            ),
            (moved, Some(IngestStatus::Done)),
            (item(7, "y.a26", "gone", serde_json::json!({})), None),
            (
                being_moved(homebrew(8, Some("Atari")), true),
                Some(IngestStatus::Moving),
            ),
            (
                being_moved(homebrew(9, Some("Atari")), false),
                Some(IngestStatus::NoDatMatch),
            ),
        ];
        for (item, expected) in cases {
            assert_eq!(status(&item), expected, "{item:?}");
        }
    }

    #[test]
    fn adding_as_additions_takes_only_items_with_one_file_and_a_system() {
        let mut two_files = homebrew(3, Some("Atari"));
        two_files
            .assessment
            .payloads
            .push(two_files.assessment.payloads[0].clone());
        let items = [
            homebrew(1, Some("Atari")),
            homebrew(2, None),
            two_files,
            fill(4),
            being_moved(homebrew(5, Some("Atari")), true),
        ];
        let bulk = choose(Decision::Add, &items, &Choices::default());
        assert_eq!(
            bulk,
            Bulk {
                items: vec![1],
                left_out: 4
            }
        );

        let mut choices = Choices::default();
        choices.additions.insert(
            2,
            NewAddition {
                system: "Atari/2600".into(),
                name: "My Hack".into(),
                parent: None,
            },
        );
        let bulk = choose(Decision::Add, &items, &choices);
        assert_eq!(bulk.items, [1, 2], "a system you chose makes it addable");
        let plan = request(Decision::Add, &bulk, &items, &choices).unwrap();
        assert_eq!(plan.action, IngestAction::Addition);
        assert_eq!(plan.items, [1, 2]);
        assert_eq!(plan.additions[&1].system, "Atari");
        assert_eq!(plan.additions[&1].name, "hack 1", "named after its file");
        assert_eq!(plan.additions[&2].name, "My Hack");
    }

    #[test]
    fn importing_dats_needs_a_provider_when_no_one_dat_names_one() {
        let items = [
            dat(1, &["No-Intro"], false),
            dat(2, &[], false),
            dat(3, &["No-Intro"], true),
            homebrew(4, Some("Atari")),
        ];
        let bulk = choose(Decision::Import, &items, &Choices::default());
        assert_eq!(bulk.items, [1]);
        let plan = request(Decision::Import, &bulk, &items, &Choices::default()).unwrap();
        assert_eq!(
            (plan.action, plan.provider),
            (IngestAction::DatImport, None)
        );

        let choices = Choices {
            provider: " Mine ".into(),
            ..Choices::default()
        };
        let bulk = choose(Decision::Import, &items, &choices);
        assert_eq!(bulk.items, [1, 2]);
        let plan = request(Decision::Import, &bulk, &items, &choices).unwrap();
        assert_eq!(plan.provider.as_deref(), Some("Mine"));
    }

    #[test]
    fn discarding_takes_anything_settled_and_dismissing_anything_waiting_on_you() {
        let items = [
            duplicate(1),
            item(2, "x.7z", "unsupported", serde_json::json!({})),
            item(3, "y.a26", "dismissed", serde_json::json!({})),
            item(4, "z.a26", "arriving", serde_json::json!({})),
            item(5, "w.zip", "failed", serde_json::json!({})),
            being_moved(duplicate(6), true),
            being_moved(duplicate(7), false),
        ];
        let discard = choose(Decision::Discard, &items, &Choices::default());
        assert_eq!(discard.items, [1, 2, 3, 7]);
        assert_eq!(discard.left_out, 3);
        let dismiss = choose(Decision::Dismiss, &items, &Choices::default());
        assert_eq!(dismiss.items, [1, 2, 7]);
        assert_eq!(
            request(Decision::Dismiss, &dismiss, &items, &Choices::default()),
            None,
            "a dismissal is no plan"
        );
        let none = choose(Decision::Discard, &items[3..4], &Choices::default());
        assert_eq!(
            request(Decision::Discard, &none, &items, &Choices::default()),
            None
        );
    }

    #[test]
    fn the_strip_counts_each_status_and_offers_what_applies_to_all_of_it() {
        let items = [
            homebrew(1, Some("Atari")),
            homebrew(2, None),
            duplicate(3),
            duplicate(4),
            being_moved(homebrew(5, Some("Atari")), true),
            fill(6),
        ];
        let strip = strip(&items, &Choices::default());
        type Shown = (IngestStatus, usize, Vec<(Decision, Vec<i64>)>);
        let shown: Vec<Shown> = strip
            .into_iter()
            .map(|c| {
                (
                    c.status,
                    c.items,
                    c.decisions.into_iter().map(|(d, b)| (d, b.items)).collect(),
                )
            })
            .collect();
        assert_eq!(
            shown,
            [
                (
                    IngestStatus::MatchesSourceSet,
                    1,
                    vec![(Decision::Dismiss, vec![6])]
                ),
                (
                    IngestStatus::Duplicate,
                    2,
                    vec![
                        (Decision::Discard, vec![3, 4]),
                        (Decision::Dismiss, vec![3, 4])
                    ]
                ),
                (
                    IngestStatus::NoDatMatch,
                    2,
                    vec![
                        (Decision::Add, vec![1]),
                        (Decision::Discard, vec![1, 2]),
                        (Decision::Dismiss, vec![1, 2])
                    ]
                ),
                (IngestStatus::Moving, 1, vec![]),
            ]
        );
    }

    #[test]
    fn labels_count_the_items() {
        assert_eq!(Decision::Add.label(1), "Add 1 as an addition");
        assert_eq!(Decision::Add.label(3), "Add 3 as additions");
        assert_eq!(Decision::Import.label(2), "Import 2 DATs");
        assert_eq!(
            Decision::Discard.left_out(2),
            "2 items can't be discarded yet: they're still arriving, being moved or gone."
        );
    }

    #[test]
    fn the_list_shows_what_is_still_in_the_folder_unless_you_choose_a_status() {
        assert!(shown(None, IngestStatus::Moving));
        assert!(!shown(None, IngestStatus::Done));
        assert!(shown(Some(IngestStatus::Done), IngestStatus::Done));
        assert!(!shown(Some(IngestStatus::Done), IngestStatus::Duplicate));
        for status in IngestStatus::ALL {
            assert_eq!(parse_slug(slug(*status)), Some(*status));
        }
        assert_eq!(parse_slug("everything"), None);
    }

    #[test]
    fn sizes_read_in_the_largest_unit_they_fill() {
        assert_eq!(size(1), "1 byte");
        assert_eq!(size(999), "999 bytes");
        assert_eq!(size(1_000), "1.0 KB");
        assert_eq!(size(999_960), "1.0 MB");
        assert_eq!(size(4_700_000_000), "4.7 GB");
    }
    #[test]
    fn a_plan_item_waits_for_approval_then_moves_then_ends_as_its_outcome_says() {
        let mut plan: IngestPlan = serde_json::from_value(serde_json::json!({
            "id": 3, "digest": "d", "created_at": "2026-01-01T00:00:00Z",
            "content": {"ingest_folder": 1, "root": "in", "action": "discard", "generation": 1,
                        "items": [], "fingerprint": []},
        }))
        .unwrap();
        let ended = |moved: bool, error: Option<&str>| IngestOutcome {
            item: 1,
            name: "x".into(),
            moved,
            detail: None,
            error: error.map(Into::into),
        };
        assert_eq!(outcome(&plan, None), Outcome::Waiting);
        plan.approval = serde_json::from_value(
            serde_json::json!({"digest": "d", "actor": "Operator", "at": "t"}),
        )
        .unwrap();
        assert_eq!(outcome(&plan, None), Outcome::Moving);
        assert_eq!(outcome(&plan, Some(&ended(true, None))), Outcome::Moved);
        assert_eq!(outcome(&plan, Some(&ended(false, None))), Outcome::Left);
        assert_eq!(
            outcome(&plan, Some(&ended(false, Some("gone")))),
            Outcome::Failed
        );
        plan.finished_at = Some("t".into());
        assert_eq!(outcome(&plan, None), Outcome::Left, "never reached");
    }

    #[test]
    fn arriving_items_say_when_they_are_next_checked() {
        let folder = |next: Option<&str>, every: Option<u64>| -> IngestFolder {
            serde_json::from_value(serde_json::json!({
                "root": "drop", "path": "/drop", "settle_secs": 60,
                "scan_every_minutes": every, "next_check_at": next,
            }))
            .unwrap()
        };
        assert_eq!(
            arriving_note(&folder(Some("2026-11-12T04:13:05.25Z"), Some(5))),
            "Assessed once settled · next check 2026-11-12 04:13:05 UTC"
        );
        assert_eq!(
            arriving_note(&folder(None, Some(1))),
            "Assessed once settled · scanned every minute"
        );
        assert_eq!(
            arriving_note(&folder(None, Some(5))),
            "Assessed once settled · scanned every 5 minutes"
        );
        assert_eq!(
            arriving_note(&folder(None, None)),
            "Assessed once settled · at the next scan"
        );
    }
}
