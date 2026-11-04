//! The library folder and build screens' pure logic: labels, the plan's items by what they do, and which library
//! folders a selection can be built into.

use title_select_api::SelectionRecord;
use title_select_api::build::{
    BlockerKind, BuildState, Cause, CleanupAction, DriftEntry, DriftKind, FileOutcome,
    IncompleteCounts, IncompleteReason, ItemKind, ItemState, KeepReason, LibraryFolder,
    PlanChoices, PlanContent, PlanItem, PlanRemoval, RemovalKind, RestoreKind, UnfinishedBuild,
};

use crate::parts::status::BuildKind;

pub fn item_kind_label(kind: ItemKind) -> &'static str {
    match kind {
        ItemKind::Copy => "Copy",
        ItemKind::Repack => "Repack",
        ItemKind::Unchanged => "Unchanged",
        ItemKind::NoSource => "No source",
        ItemKind::Conflict => "Conflict",
        ItemKind::Blocked => "Blocked",
        ItemKind::Adopt => "Adopt",
        ItemKind::Unresolved => "Unresolved",
        ItemKind::Remove => "Remove",
        ItemKind::KeepAside => "Keep aside",
        ItemKind::Acknowledge => "Acknowledge",
        ItemKind::Restore => "Restore",
    }
}

/// What a group of items means, for the heading over them.
pub fn item_kind_note(kind: ItemKind) -> &'static str {
    match kind {
        ItemKind::Copy => "Copied as they are: the source is already this game's TorrentZip.",
        ItemKind::Repack => "Written as a TorrentZip from the verified ROMs.",
        ItemKind::Unchanged => "Already in the folder, built from the same ROMs.",
        ItemKind::NoSource => "No audited file holds every ROM, so these are skipped.",
        ItemKind::Conflict => {
            "A file the store did not build, or one changed since it was built, is at the target. It is never \
             overwritten, so these are skipped unless you choose to keep the file aside and rebuild."
        }
        ItemKind::Blocked => "These cannot be built as planned, so they are skipped.",
        ItemKind::Adopt => {
            "Files already in the folder that hold the ROMs the DAT expects. You chose to adopt them: they are \
             recorded as built, and nothing is written."
        }
        ItemKind::Unresolved => {
            "Their groups are unresolved, and you build the resolved groups only. Whatever is at their targets is \
             left untouched."
        }
        ItemKind::Remove => {
            "Built files no longer selected: each is kept aside, or forgotten if it is gone."
        }
        ItemKind::KeepAside => "Moved, whole, into the retention area as kept files.",
        ItemKind::Acknowledge => "Left as they are, and no longer reported until they change.",
        ItemKind::Restore => "Kept files put back into the folder.",
    }
}

pub fn incomplete_reason_label(reason: IncompleteReason) -> &'static str {
    match reason {
        IncompleteReason::Unresolved => "unresolved",
        IncompleteReason::NoSource => "no source",
        IncompleteReason::Failed => "failed",
        IncompleteReason::Conflict => "conflict",
        IncompleteReason::Blocked => "blocked",
        IncompleteReason::Missing => "missing",
        IncompleteReason::MissingDisc => "missing disc",
    }
}

/// The counts behind an incomplete badge, such as `2 conflict, 1 failed`, leaving out reasons with none.
pub fn incomplete_counts(counts: &IncompleteCounts) -> String {
    IncompleteReason::ALL
        .into_iter()
        .filter(|r| counts.get(*r) > 0)
        .map(|r| format!("{} {}", counts.get(r), incomplete_reason_label(r)))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The interrupted-build badge's text, from the store's record of the build.
pub fn unfinished_label(unfinished: &UnfinishedBuild) -> String {
    let what = match unfinished.state {
        BuildState::Failed => "Failed build",
        _ => "Interrupted build",
    };
    let kept = match unfinished.kept {
        0 => String::new(),
        n => format!(", {n} kept aside"),
    };
    format!(
        "{what}: {} applied, {} uncertain{kept}",
        unfinished.applied, unfinished.uncertain
    )
}

pub fn file_outcome_label(outcome: FileOutcome) -> &'static str {
    match outcome {
        FileOutcome::Unchanged => "Unchanged",
        FileOutcome::Refreshed => "Unchanged, record refreshed",
        FileOutcome::Repacked => "Same ROMs, another ZIP",
        FileOutcome::Conflict => "Conflict",
        FileOutcome::Missing => "Missing",
        FileOutcome::Unreadable => "Unreadable",
    }
}

pub fn drift_kind_label(kind: DriftKind) -> &'static str {
    match kind {
        DriftKind::Unselected => "Known title, not selected",
        DriftKind::Selected => "Selected title, not built here",
        DriftKind::Renamed => "Known title under another name",
        DriftKind::Unknown => "Unknown file",
        DriftKind::Directory => "Directory, never opened",
    }
}

pub fn cleanup_action_label(action: CleanupAction) -> &'static str {
    match action {
        CleanupAction::KeepAside => "Keep aside",
        CleanupAction::Adopt => "Adopt",
        CleanupAction::Acknowledge => "Acknowledge",
        CleanupAction::AddAddition => "Add to additions",
    }
}

pub fn restore_kind_label(kind: RestoreKind) -> &'static str {
    match kind {
        RestoreKind::Restore => "Put back and record as built",
        RestoreKind::Adopt => "Adopt under its game's file name",
        RestoreKind::PutBack => "Put back as it was",
    }
}

/// What a cleanup can do with drift entry `d`: keep it aside, always; adopt it, only a file that names a selected
/// title or holds a known one under another name; add it as an addition, only a file no DAT knows;
/// acknowledge it, unless it is at a selected game's file name.
pub fn cleanup_actions(d: &DriftEntry) -> Vec<CleanupAction> {
    let mut actions = vec![CleanupAction::KeepAside];
    if d.kind == DriftKind::Unknown {
        actions.push(CleanupAction::AddAddition);
    }
    if d.game.is_some() && matches!(d.kind, DriftKind::Selected | DriftKind::Renamed) {
        actions.push(CleanupAction::Adopt);
    }
    if !d.conflict {
        actions.push(CleanupAction::Acknowledge);
    }
    actions
}

/// What you choose for a build plan's item in conflict: leave it, adopt the file there, or keep it aside and build
/// the game in its place.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConflictChoice {
    Leave,
    Adopt,
    KeepAside,
}

impl ConflictChoice {
    pub fn label(self) -> &'static str {
        match self {
            ConflictChoice::Leave => "Leave it",
            ConflictChoice::Adopt => "Adopt it",
            ConflictChoice::KeepAside => "Keep aside and rebuild",
        }
    }

    /// The choice `choices` makes for `item`.
    pub fn of(item: &PlanItem, choices: &PlanChoices) -> Self {
        if choices.adopt.contains(&item.target) {
            ConflictChoice::Adopt
        } else if choices.keep_aside.contains(&item.target) {
            ConflictChoice::KeepAside
        } else {
            ConflictChoice::Leave
        }
    }

    /// The choices `item` offers: none unless it is in conflict, or was and you chose for it.
    pub fn offered(item: &PlanItem) -> Vec<Self> {
        let adopt = item.adoptable || item.kind == ItemKind::Adopt;
        let keep = item.keepable
            || item
                .keep
                .as_ref()
                .is_some_and(|k| k.reason == KeepReason::Conflict);
        let leave = (adopt || keep).then_some(ConflictChoice::Leave);
        let adopt = adopt.then_some(ConflictChoice::Adopt);
        leave
            .into_iter()
            .chain(adopt)
            .chain(keep.then_some(ConflictChoice::KeepAside))
            .collect()
    }
}

/// The choices of a plan made again with `picked`, a choice by target, keeping the rest.
pub fn with_conflict_choices<'a>(
    choices: &PlanChoices,
    picked: impl IntoIterator<Item = (&'a String, &'a ConflictChoice)>,
) -> PlanChoices {
    let mut next = choices.clone();
    for (target, choice) in picked {
        next.adopt.retain(|t| t != target);
        next.keep_aside.retain(|t| t != target);
        match choice {
            ConflictChoice::Leave => {}
            ConflictChoice::Adopt => next.adopt.push(target.clone()),
            ConflictChoice::KeepAside => next.keep_aside.push(target.clone()),
        }
    }
    next.adopt.sort();
    next.keep_aside.sort();
    next
}

/// An item's row in its plan's review, as current → proposed: where its file comes from and its name, or, for an
/// item that writes nothing, its name and what becomes of it.
pub fn item_change(item: &PlanItem) -> (String, String) {
    let target = item.target.clone();
    match item.kind {
        ItemKind::Copy | ItemKind::Repack => {
            let mut from: Vec<String> = item
                .sources
                .iter()
                .map(|s| match &s.member {
                    Some(member) => format!("{} ({member})", s.path),
                    None => s.path.clone(),
                })
                .collect();
            from.dedup();
            (from.join(", "), target)
        }
        ItemKind::Unchanged => (target, "as it is".into()),
        ItemKind::Adopt => (target, "recorded as built".into()),
        _ => (target, "skipped".into()),
    }
}

/// The kind an item's row counts under, and what else it does: an item that keeps a file aside first (a replacement,
/// or a conflict you chose to keep aside) also counts as set aside, as the plan's summary counts it.
pub fn item_kinds(item: &PlanItem) -> (BuildKind, Option<BuildKind>) {
    (
        item.kind.into(),
        item.keep.as_ref().map(|_| BuildKind::SetAside),
    )
}

/// The kind a removal's row counts under, and what else it does: one that keeps the file also counts as set aside.
pub fn removal_kinds(removal: &PlanRemoval) -> (BuildKind, Option<BuildKind>) {
    let kept = removal.kind == RemovalKind::Keep;
    (BuildKind::Remove, kept.then_some(BuildKind::SetAside))
}

/// The kind a cleanup's entry counts under, by what it does with it.
pub fn entry_kind(action: CleanupAction) -> BuildKind {
    match action {
        CleanupAction::KeepAside | CleanupAction::AddAddition => BuildKind::SetAside,
        CleanupAction::Adopt => BuildKind::Adopt,
        CleanupAction::Acknowledge => BuildKind::Acknowledge,
    }
}

/// The kind a cleanup's kept file put back counts under: one adopted under its game's name is an adoption.
pub fn restore_kind(kind: RestoreKind) -> BuildKind {
    match kind {
        RestoreKind::Adopt => BuildKind::Adopt,
        RestoreKind::Restore | RestoreKind::PutBack => BuildKind::Restore,
    }
}

/// The kinds a plan's rows count under, each with what else it does, in the order its review lists them.
pub fn plan_kinds(content: &PlanContent) -> Vec<(BuildKind, Option<BuildKind>)> {
    let Some(cleanup) = &content.cleanup else {
        let items = content.items.iter().map(item_kinds);
        return items
            .chain(content.removals.iter().map(removal_kinds))
            .collect();
    };
    let entries = cleanup
        .entries
        .iter()
        .filter_map(|e| e.action)
        .map(|a| (entry_kind(a), None));
    let restores = cleanup
        .restores
        .iter()
        .map(|r| (restore_kind(r.kind), None));
    let repacks = cleanup
        .repacks
        .iter()
        .map(|_| (BuildKind::Repack, Some(BuildKind::SetAside)));
    entries.chain(restores).chain(repacks).collect()
}

pub fn keep_reason_label(reason: KeepReason) -> &'static str {
    match reason {
        KeepReason::Removal => "no longer selected",
        KeepReason::Replacement => "replaced",
        KeepReason::Conflict => "conflict kept aside",
        KeepReason::Drift => "not built here, kept aside",
        KeepReason::Repack => "before repacking",
    }
}

pub fn cause_label(cause: Cause) -> &'static str {
    match cause {
        Cause::Settings => "your settings",
        Cause::Choices => "your choices",
        Cause::DatRevision => "the DAT revision",
    }
}

pub fn blocker_label(kind: BlockerKind) -> &'static str {
    match kind {
        BlockerKind::NoAudit => "No audit",
        BlockerKind::RevisionMismatch => "DAT revision mismatch",
        BlockerKind::Unresolved => "Unresolved group",
        BlockerKind::ReadOnlyRoot => "Read-only root",
        BlockerKind::FolderMissing => "Folder missing",
        BlockerKind::Marker => "Marker",
        BlockerKind::Replacement => "Replacement",
        BlockerKind::Removal => "Removal",
        BlockerKind::Recovery => "Recovery",
        BlockerKind::Cleanup => "Cleanup",
    }
}

pub fn build_state_label(state: BuildState) -> &'static str {
    match state {
        BuildState::Running => "Running",
        BuildState::Succeeded => "Succeeded",
        BuildState::Failed => "Failed",
        BuildState::Interrupted => "Interrupted",
    }
}

pub fn item_state_label(state: ItemState) -> &'static str {
    match state {
        ItemState::Pending => "Not written",
        ItemState::Publishing => "Writing",
        ItemState::Applied => "Written",
        ItemState::Failed => "Failed",
        ItemState::Uncertain => "Uncertain",
    }
}

/// The plan's items by kind, the ones a build writes first, leaving out kinds with none.
pub fn by_kind(items: &[PlanItem]) -> Vec<(ItemKind, Vec<&PlanItem>)> {
    ItemKind::ALL
        .into_iter()
        .map(|kind| (kind, items.iter().filter(|i| i.kind == kind).collect()))
        .filter(|(_, items): &(ItemKind, Vec<&PlanItem>)| !items.is_empty())
        .collect()
}

/// The library folders `selection` can be built into: those bound to its source set and selection profile.
pub fn folders_for<'a>(
    folders: &'a [LibraryFolder],
    selection: &SelectionRecord,
) -> Vec<&'a LibraryFolder> {
    folders
        .iter()
        .filter(|f| f.source_set == selection.source_set && f.profile == selection.profile.name)
        .collect()
}

/// Where you confirm an unresolved group: its selection, opened at the group.
pub fn group_href(selection: i64, group: &str) -> String {
    format!("/selections/{selection}?group={}", url_component(group))
}

/// Percent-encodes everything but unreserved characters, as `encodeURIComponent` does.
fn url_component(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for b in text.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use title_select_api::ProfileVersionRef;
    use title_select_api::build::{Cleanup, KeepAside};
    use title_select_api::selection::{SelectionPins, SelectionSummary};

    fn drift(kind: DriftKind, game: Option<&str>, conflict: bool) -> DriftEntry {
        DriftEntry {
            name: "x.zip".into(),
            kind,
            game: game.map(Into::into),
            conflict,
            hashed: false,
            detail: None,
        }
    }

    #[test]
    fn a_cleanup_offers_adopting_only_selected_or_renamed_titles_and_never_acknowledges_a_conflict()
    {
        use CleanupAction::*;
        assert_eq!(
            cleanup_actions(&drift(DriftKind::Selected, Some("Pong"), true)),
            [KeepAside, Adopt]
        );
        assert_eq!(
            cleanup_actions(&drift(DriftKind::Renamed, Some("Pong"), false)),
            [KeepAside, Adopt, Acknowledge]
        );
        assert_eq!(
            cleanup_actions(&drift(DriftKind::Unselected, Some("Pong"), false)),
            [KeepAside, Acknowledge]
        );
        assert_eq!(
            cleanup_actions(&drift(DriftKind::Directory, None, false)),
            [KeepAside, Acknowledge]
        );
    }

    fn item(game: &str, kind: ItemKind) -> PlanItem {
        PlanItem {
            group: game.to_lowercase(),
            game: game.into(),
            target: format!("{game}.zip"),
            kind,
            roms: Vec::new(),
            sources: Vec::new(),
            reason: None,
            existing: None,
            keepable: false,
            keep: None,
            adoptable: false,
            loose: false,
            chd: false,
            converts: None,
            addition: None,
        }
    }

    fn folder(id: i64, source_set: &str, profile: &str) -> LibraryFolder {
        LibraryFolder {
            id,
            root: "play".into(),
            path: "atari2600".into(),
            system: "No-Intro/Atari - Atari 2600".into(),
            source_set: source_set.into(),
            profile: profile.into(),
            generation: 0,
            created_at: String::new(),
            writable: true,
            build: None,
            files: 0,
            active_job: None,
            latest_plan: None,
            incomplete: None,
            unfinished: None,
            check: None,
            plan_open: false,
            stale: Vec::new(),
        }
    }

    #[test]
    fn items_are_grouped_by_kind_with_written_ones_first_and_empty_kinds_left_out() {
        let items = [
            item("Pong", ItemKind::NoSource),
            item("Combat", ItemKind::Repack),
            item("Yars", ItemKind::Copy),
            item("Adventure", ItemKind::Repack),
        ];

        let grouped = by_kind(&items);

        let shape: Vec<(ItemKind, Vec<&str>)> = grouped
            .iter()
            .map(|(k, items)| (*k, items.iter().map(|i| i.game.as_str()).collect()))
            .collect();
        assert_eq!(
            shape,
            [
                (ItemKind::Copy, vec!["Yars"]),
                (ItemKind::Repack, vec!["Combat", "Adventure"]),
                (ItemKind::NoSource, vec!["Pong"]),
            ]
        );
    }

    #[test]
    fn a_selection_is_offered_the_folders_bound_to_its_source_set_and_profile() {
        let folders = [
            folder(1, "Atari", "Mine"),
            folder(2, "Atari", "Theirs"),
            folder(3, "Other", "Mine"),
            folder(4, "Atari", "Mine"),
        ];
        let selection = SelectionRecord {
            id: 7,
            source_set: "Atari".into(),
            dat_revision_sha256: String::new(),
            dat_revision_version: String::new(),
            profile: ProfileVersionRef {
                name: "Mine".into(),
                version: 1,
            },
            latest_profile_version: 1,
            job: None,
            system: String::new(),
            system_settings: false,
            pins: SelectionPins::default(),
            summary: SelectionSummary::default(),
            created_at: String::new(),
            pruned: false,
            library_folders: Vec::new(),
        };

        let ids: Vec<i64> = folders_for(&folders, &selection)
            .iter()
            .map(|f| f.id)
            .collect();

        assert_eq!(ids, [1, 4]);
    }

    #[test]
    fn an_incomplete_badge_counts_only_the_reasons_it_has() {
        let counts = IncompleteCounts {
            conflict: 2,
            failed: 1,
            ..IncompleteCounts::default()
        };

        assert_eq!(incomplete_counts(&counts), "1 failed, 2 conflict");
    }

    #[test]
    fn choosing_for_conflicts_keeps_the_other_choices() {
        let choices = PlanChoices {
            resolved_only: true,
            adopt: vec!["Pong.zip".into()],
            keep_aside: vec!["Combat.zip".into()],
            remove_all: false,
        };
        let picked = BTreeMap::from([
            ("Combat.zip".to_string(), ConflictChoice::Adopt),
            ("Pong.zip".to_string(), ConflictChoice::Leave),
            ("Asteroids.zip".to_string(), ConflictChoice::KeepAside),
        ]);

        let next = with_conflict_choices(&choices, &picked);

        assert_eq!(next.adopt, ["Combat.zip"]);
        assert_eq!(next.keep_aside, ["Asteroids.zip"]);
        assert!(next.resolved_only);
    }

    #[test]
    fn only_a_conflict_or_an_item_chosen_for_one_offers_choices() {
        assert!(ConflictChoice::offered(&item("Pong", ItemKind::NoSource)).is_empty());
        let conflict = PlanItem {
            keepable: true,
            ..item("Pong", ItemKind::Conflict)
        };
        assert_eq!(
            ConflictChoice::offered(&conflict),
            [ConflictChoice::Leave, ConflictChoice::KeepAside]
        );
        assert_eq!(
            ConflictChoice::offered(&item("Pong", ItemKind::Adopt)),
            [ConflictChoice::Leave, ConflictChoice::Adopt]
        );
        let choices = PlanChoices {
            adopt: vec!["Pong.zip".into()],
            ..PlanChoices::default()
        };
        assert_eq!(
            ConflictChoice::of(&conflict, &choices),
            ConflictChoice::Adopt
        );
    }

    #[test]
    fn a_written_item_reads_from_its_sources_and_a_skipped_one_from_its_name() {
        let copy = PlanItem {
            sources: ["a.zip", "a.zip", "b.zip"]
                .map(|path| {
                    serde_json::from_value(serde_json::json!({
                        "rom": "r", "path": path, "file": { "size": 1, "mtime_ns": 0 }
                    }))
                    .unwrap()
                })
                .into(),
            ..item("Pong", ItemKind::Copy)
        };
        assert_eq!(
            item_change(&copy),
            ("a.zip, b.zip".into(), "Pong.zip".into())
        );
        assert_eq!(
            item_change(&item("Pong", ItemKind::Conflict)),
            ("Pong.zip".into(), "skipped".into())
        );
    }

    #[test]
    fn an_unresolved_group_links_to_its_selection_with_the_name_escaped() {
        assert_eq!(
            group_href(7, "café & co/1"),
            "/selections/7?group=caf%C3%A9%20%26%20co%2F1"
        );
    }

    /// The kinds counted from a plan's rows, as `kind_counts` counts them for the review's strip.
    fn counted(content: &PlanContent) -> Vec<(BuildKind, usize)> {
        let kinds = plan_kinds(content);
        crate::parts::plan_model::kind_counts(
            kinds
                .into_iter()
                .flat_map(|(k, also)| [Some(k), also])
                .flatten(),
        )
    }

    fn summary_of(content: &PlanContent, kind: BuildKind) -> usize {
        counted(content)
            .into_iter()
            .find(|(k, _)| *k == kind)
            .map_or(0, |(_, n)| n)
    }

    fn content(items: Vec<PlanItem>, removals: Vec<PlanRemoval>) -> PlanContent {
        serde_json::from_value::<PlanContent>(serde_json::json!({
            "folder": 1, "generation": 1, "selection": 1, "audit": null, "dat_revision": "r",
            "items": [], "blockers": [],
        }))
        .map(|c| PlanContent {
            items,
            removals,
            ..c
        })
        .unwrap()
    }

    #[test]
    fn a_replacement_counts_as_set_aside_as_the_summary_does() {
        let kept: KeepAside = serde_json::from_value(serde_json::json!({
            "reason": "replacement", "name": "Pong.zip",
            "file": { "size": 1, "mtime_ns": 0 }, "identity": { "dev": 1, "ino": 2 },
        }))
        .unwrap();
        let removal = |kind| PlanRemoval {
            target: "Combat.zip".into(),
            game: "Combat".into(),
            kind,
            reason: String::new(),
            keep: None,
            converts_to: None,
        };
        let plan = content(
            vec![
                PlanItem {
                    keep: Some(kept),
                    ..item("Pong", ItemKind::Repack)
                },
                item("Combat", ItemKind::Copy),
            ],
            vec![removal(RemovalKind::Keep), removal(RemovalKind::Forget)],
        );
        let summary = plan.summary();

        assert_eq!(summary.keep, 2);
        assert_eq!(summary_of(&plan, BuildKind::SetAside), 2);
        assert_eq!(summary_of(&plan, BuildKind::Repack), 1);
        assert_eq!(summary_of(&plan, BuildKind::Remove), 2);
        assert_eq!(
            plan_kinds(&plan).len(),
            4,
            "one row each, counted twice where it sets a file aside"
        );
    }

    #[test]
    fn a_cleanups_rows_count_as_its_summary_does() {
        let stat = serde_json::json!({ "file": { "size": 1, "mtime_ns": 0 }, "identity": { "dev": 1, "ino": 2 } });
        let entry = |name: &str, action: &str| {
            let mut e = serde_json::json!({ "name": name, "kind": "unselected", "action": action });
            e.as_object_mut()
                .unwrap()
                .extend(stat.as_object().unwrap().clone());
            e
        };
        let mut kept = serde_json::json!({
            "id": 1, "folder": 1, "generation": 1, "name": "Pong.zip", "kept_name": "Pong.zip",
            "reason": "drift", "build": 1, "kept_at": "now",
        });
        kept.as_object_mut()
            .unwrap()
            .extend(stat.as_object().unwrap().clone());
        let mut repack = serde_json::json!({ "target": "Combat.zip", "game": "Combat", "roms": [], "sha256": "s" });
        repack
            .as_object_mut()
            .unwrap()
            .extend(stat.as_object().unwrap().clone());
        let cleanup: Cleanup = serde_json::from_value(serde_json::json!({
            "build": 1, "choices": {},
            "entries": [entry("a", "keep_aside"), entry("b", "add_addition"), entry("c", "adopt"), entry("d", "acknowledge")],
            "restores": [{ "kept": kept, "kind": "adopt", "target": "Pong.zip" }],
            "repacks": [repack],
        }))
        .unwrap();
        let plan = PlanContent {
            cleanup: Some(cleanup),
            ..content(Vec::new(), Vec::new())
        };
        let s = plan.summary();

        for (kind, n) in [
            (BuildKind::SetAside, s.keep),
            (BuildKind::Adopt, s.adopt),
            (BuildKind::Acknowledge, s.acknowledge),
            (BuildKind::Restore, s.restore),
            (BuildKind::Repack, s.repack),
        ] {
            assert_eq!(summary_of(&plan, kind) as u64, n, "{kind:?}");
        }
        assert_eq!(s.keep, 3);
    }
}
