//! The library folder and build screens' pure logic: labels, the plan's items by what they do, and which library
//! folders a selection can be built into.

use title_select_api::SelectionRecord;
use title_select_api::build::{
    BlockerKind, BuildState, Cause, CleanupAction, DriftEntry, DriftKind, FileOutcome,
    IncompleteCounts, IncompleteReason, ItemKind, ItemState, KeepReason, LibraryFolder,
    PlanChoices, PlanItem, RemovalKind, RestoreKind, UnfinishedBuild,
};

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

/// The choices of a plan made again with `adopt` for `target` set to `on`, keeping the rest.
pub fn with_adopt(choices: &PlanChoices, target: &str, on: bool) -> PlanChoices {
    let mut next = choices.clone();
    next.adopt.retain(|t| t != target);
    if on {
        next.keep_aside.retain(|t| t != target);
        next.adopt.push(target.to_owned());
        next.adopt.sort();
    }
    next
}

/// The choices of a plan made again with keeping aside the file at `target` set to `on`, keeping the rest.
pub fn with_keep_aside(choices: &PlanChoices, target: &str, on: bool) -> PlanChoices {
    let mut next = choices.clone();
    next.keep_aside.retain(|t| t != target);
    if on {
        next.adopt.retain(|t| t != target);
        next.keep_aside.push(target.to_owned());
        next.keep_aside.sort();
    }
    next
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

pub fn removal_kind_label(kind: RemovalKind) -> &'static str {
    match kind {
        RemovalKind::Keep => "Keep aside",
        RemovalKind::Forget => "Forget",
        RemovalKind::Leave => "Leave",
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
    use title_select_api::ProfileVersionRef;
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
    fn choosing_to_adopt_a_conflict_keeps_the_other_choices() {
        let choices = PlanChoices {
            resolved_only: true,
            adopt: vec!["Pong.zip".into()],
            keep_aside: vec!["Combat.zip".into()],
        };

        let both = with_adopt(&choices, "Combat.zip", true);
        let left = with_adopt(&both, "Pong.zip", false);

        assert_eq!(both.adopt, ["Combat.zip", "Pong.zip"]);
        assert!(both.keep_aside.is_empty());
        assert_eq!(left.adopt, ["Combat.zip"]);
        assert!(left.resolved_only);
    }

    #[test]
    fn an_unresolved_group_links_to_its_selection_with_the_name_escaped() {
        assert_eq!(
            group_href(7, "café & co/1"),
            "/selections/7?group=caf%C3%A9%20%26%20co%2F1"
        );
    }
}
