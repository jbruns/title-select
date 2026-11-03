//! The status vocabulary: each status the workbench shows has a glyph, a tone and a short label, so colour is
//! always paired with shape. A list shows its statuses with `StatusIcon` or `StatusBadge` and explains them with one
//! `Legend` (see `parts::legend`).

use leptos::prelude::*;
use title_select_api::JobState;
use title_select_api::build::ItemKind;
use title_select_api::repair::RepairKind;
use title_select_api::report::{Finding, GameStatus};

use super::icon::{Glyph, Icon};

/// A status's colour, named by meaning. Each is a `--tone-*` token in `style/main.css` with a light and a dark value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Tone {
    Good,
    Review,
    Failed,
    Active,
    Neutral,
}

impl Tone {
    pub const ALL: [Tone; 5] = [
        Tone::Good,
        Tone::Review,
        Tone::Failed,
        Tone::Active,
        Tone::Neutral,
    ];

    /// The class that colours an element with the tone.
    pub fn class(self) -> &'static str {
        match self {
            Tone::Good => "tone-good",
            Tone::Review => "tone-review",
            Tone::Failed => "tone-failed",
            Tone::Active => "tone-active",
            Tone::Neutral => "tone-neutral",
        }
    }

    /// The CSS custom property holding the tone's colour.
    pub fn token(self) -> &'static str {
        match self {
            Tone::Good => "--tone-good",
            Tone::Review => "--tone-review",
            Tone::Failed => "--tone-failed",
            Tone::Active => "--tone-active",
            Tone::Neutral => "--tone-neutral",
        }
    }
}

/// How a status looks: its glyph and tone, a label of three words or fewer for cells and legends, and a hint of one
/// sentence for its tooltip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Look {
    pub glyph: Glyph,
    pub tone: Tone,
    pub label: &'static str,
    pub hint: &'static str,
}

/// One closed set of statuses. `ALL` lists them in the order legends show them; for row statuses it is also their
/// precedence, highest first.
pub trait Status: Copy + Eq + Send + Sync + 'static {
    const ALL: &'static [Self];

    fn look(self) -> Look;

    /// Where the status comes in `ALL`.
    fn rank(self) -> usize {
        Self::ALL
            .iter()
            .position(|s| *s == self)
            .expect("every status is in ALL")
    }
}

/// The status of highest precedence among those that apply.
pub fn highest<S: Status>(applying: impl IntoIterator<Item = S>) -> Option<S> {
    applying.into_iter().min_by_key(|s| s.rank())
}

const fn look(glyph: Glyph, tone: Tone, label: &'static str, hint: &'static str) -> Look {
    Look {
        glyph,
        tone,
        label,
        hint,
    }
}

/// An Entry's Finding, with Verified for a Matched Entry read on Payload Evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FindingStatus {
    Verified,
    Matched,
    Misnamed,
    Conflicting,
    Mismatched,
    Unmatched,
    Unreadable,
}

impl FindingStatus {
    /// The status of an Entry with `finding`; only payload Evidence makes a Matched Entry Verified.
    pub fn of(finding: Finding, payload: bool) -> Self {
        match finding {
            Finding::Matched if payload => FindingStatus::Verified,
            Finding::Matched => FindingStatus::Matched,
            Finding::Misnamed => FindingStatus::Misnamed,
            Finding::Conflicting => FindingStatus::Conflicting,
            Finding::Mismatched => FindingStatus::Mismatched,
            Finding::Unmatched => FindingStatus::Unmatched,
            Finding::Unreadable => FindingStatus::Unreadable,
        }
    }
}

impl Status for FindingStatus {
    const ALL: &'static [Self] = &[
        FindingStatus::Verified,
        FindingStatus::Matched,
        FindingStatus::Misnamed,
        FindingStatus::Conflicting,
        FindingStatus::Mismatched,
        FindingStatus::Unmatched,
        FindingStatus::Unreadable,
    ];

    fn look(self) -> Look {
        match self {
            FindingStatus::Verified => look(
                Glyph::ShieldCheck,
                Tone::Good,
                "Verified",
                "Matched, and every byte of its payload was read.",
            ),
            FindingStatus::Matched => look(
                Glyph::CircleCheck,
                Tone::Good,
                "Matched",
                "Its payload matches the ROM its name gives.",
            ),
            FindingStatus::Misnamed => look(
                Glyph::Tag,
                Tone::Review,
                "Misnamed",
                "Its payload matches a ROM, but its name isn't one in the DAT revision.",
            ),
            FindingStatus::Conflicting => look(
                Glyph::ArrowLeftRight,
                Tone::Review,
                "Conflicting",
                "Its name is one ROM, but its payload matches another.",
            ),
            FindingStatus::Mismatched => look(
                Glyph::EqualNot,
                Tone::Review,
                "Mismatched",
                "Its name is a ROM, but its payload matches none. This isn't proof of damage.",
            ),
            FindingStatus::Unmatched => look(
                Glyph::CircleQuestionMark,
                Tone::Review,
                "Unmatched",
                "Neither its name nor its payload matches a ROM.",
            ),
            FindingStatus::Unreadable => look(
                Glyph::FileExclamationPoint,
                Tone::Failed,
                "Unreadable",
                "It couldn't be read. Its detail says why.",
            ),
        }
    }
}

impl From<Finding> for FindingStatus {
    fn from(finding: Finding) -> Self {
        FindingStatus::of(finding, false)
    }
}

impl Status for GameStatus {
    const ALL: &'static [Self] = &GameStatus::ALL;

    fn look(self) -> Look {
        match self {
            GameStatus::Complete => look(
                Glyph::CircleCheck,
                Tone::Good,
                "Complete",
                "Every ROM is present.",
            ),
            GameStatus::Incomplete => look(
                Glyph::CircleDashed,
                Tone::Review,
                "Incomplete",
                "Some of its ROMs are present.",
            ),
            GameStatus::Missing => look(
                Glyph::CircleOff,
                Tone::Neutral,
                "Missing",
                "None of its ROMs is present.",
            ),
        }
    }
}

/// A Source Set's row status, highest precedence first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SourceSetStatus {
    Running,
    RepairOpen,
    NeedsReview,
    Stale,
    Verified,
    NeverAudited,
}

/// What is true of a Source Set, from which its row status follows.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SourceSetFacts {
    /// A Job is queued or running for it.
    pub running: bool,
    /// A Repair plan is proposed and not yet approved, or approved and not yet finished.
    pub repair_open: bool,
    /// Its latest Audit has Needs Review Entries.
    pub needs_review: bool,
    pub stale: bool,
    pub audited: bool,
}

impl SourceSetStatus {
    pub fn of(facts: SourceSetFacts) -> Self {
        let applying = [
            (facts.running, SourceSetStatus::Running),
            (facts.repair_open, SourceSetStatus::RepairOpen),
            (
                facts.audited && facts.needs_review,
                SourceSetStatus::NeedsReview,
            ),
            (facts.audited && facts.stale, SourceSetStatus::Stale),
            (facts.audited, SourceSetStatus::Verified),
            (true, SourceSetStatus::NeverAudited),
        ];
        highest(applying.into_iter().filter(|(a, _)| *a).map(|(_, s)| s))
            .unwrap_or(SourceSetStatus::NeverAudited)
    }
}

impl Status for SourceSetStatus {
    const ALL: &'static [Self] = &[
        SourceSetStatus::Running,
        SourceSetStatus::RepairOpen,
        SourceSetStatus::NeedsReview,
        SourceSetStatus::Stale,
        SourceSetStatus::Verified,
        SourceSetStatus::NeverAudited,
    ];

    fn look(self) -> Look {
        match self {
            SourceSetStatus::Running => look(
                Glyph::LoaderCircle,
                Tone::Active,
                "Running",
                "A job is running for it.",
            ),
            SourceSetStatus::RepairOpen => look(
                Glyph::Wrench,
                Tone::Active,
                "Repair open",
                "A repair is planned or under way.",
            ),
            SourceSetStatus::NeedsReview => look(
                Glyph::TriangleAlert,
                Tone::Review,
                "Needs review",
                "Its latest audit found entries for you to review.",
            ),
            SourceSetStatus::Stale => look(
                Glyph::ClockAlert,
                Tone::Review,
                "Stale",
                "Its files look changed since its audit, or a newer DAT revision is waiting.",
            ),
            SourceSetStatus::Verified => look(
                Glyph::ShieldCheck,
                Tone::Good,
                "Verified",
                "Its latest audit found nothing to review.",
            ),
            SourceSetStatus::NeverAudited => look(
                Glyph::ShieldQuestionMark,
                Tone::Neutral,
                "Never audited",
                "It hasn't been audited yet.",
            ),
        }
    }
}

/// A Library Folder's row status, highest precedence first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LibraryStatus {
    Running,
    PlanOpen,
    Drift,
    Incomplete,
    Stale,
    Current,
    NeverBuilt,
}

/// What is true of a Library Folder, from which its row status follows.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LibraryFacts {
    /// A Job is queued or running for it.
    pub running: bool,
    /// A Build plan is proposed and not yet approved, or approved and not yet finished.
    pub plan_open: bool,
    pub drift: bool,
    pub incomplete: bool,
    pub stale: bool,
    pub built: bool,
}

impl LibraryStatus {
    pub fn of(facts: LibraryFacts) -> Self {
        let applying = [
            (facts.running, LibraryStatus::Running),
            (facts.plan_open, LibraryStatus::PlanOpen),
            (facts.built && facts.drift, LibraryStatus::Drift),
            (facts.built && facts.incomplete, LibraryStatus::Incomplete),
            (facts.built && facts.stale, LibraryStatus::Stale),
            (facts.built, LibraryStatus::Current),
            (true, LibraryStatus::NeverBuilt),
        ];
        highest(applying.into_iter().filter(|(a, _)| *a).map(|(_, s)| s))
            .unwrap_or(LibraryStatus::NeverBuilt)
    }
}

impl Status for LibraryStatus {
    const ALL: &'static [Self] = &[
        LibraryStatus::Running,
        LibraryStatus::PlanOpen,
        LibraryStatus::Drift,
        LibraryStatus::Incomplete,
        LibraryStatus::Stale,
        LibraryStatus::Current,
        LibraryStatus::NeverBuilt,
    ];

    fn look(self) -> Look {
        match self {
            LibraryStatus::Running => look(
                Glyph::LoaderCircle,
                Tone::Active,
                "Running",
                "A job is running for it.",
            ),
            LibraryStatus::PlanOpen => look(
                Glyph::ClipboardList,
                Tone::Active,
                "Plan open",
                "A build is planned or under way.",
            ),
            LibraryStatus::Drift => look(
                Glyph::FileDiff,
                Tone::Review,
                "Drift",
                "Its files differ from its current build.",
            ),
            LibraryStatus::Incomplete => look(
                Glyph::CircleDashed,
                Tone::Review,
                "Incomplete",
                "Its current build lacks games its selection chose.",
            ),
            LibraryStatus::Stale => look(
                Glyph::ClockAlert,
                Tone::Review,
                "Stale",
                "Its source set or selection profile changed since its build.",
            ),
            LibraryStatus::Current => look(
                Glyph::CircleCheck,
                Tone::Good,
                "Current",
                "It holds exactly its current build.",
            ),
            LibraryStatus::NeverBuilt => look(
                Glyph::CircleDotDashed,
                Tone::Neutral,
                "Never built",
                "It hasn't been built yet.",
            ),
        }
    }
}

/// The kind of a plan item, shown beside its *current → proposed* row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PlanKind {
    Rename,
    Repack,
    Fill,
    Replace,
    SetAside,
    Convert,
}

impl From<RepairKind> for PlanKind {
    fn from(kind: RepairKind) -> Self {
        match kind {
            // Resolving a Conflicting Entry writes its payload under the name of the ROM it matches.
            RepairKind::Rename | RepairKind::Conflict => PlanKind::Rename,
            RepairKind::Repack => PlanKind::Repack,
            RepairKind::Fill => PlanKind::Fill,
            RepairKind::Replace => PlanKind::Replace,
            RepairKind::KeepAside => PlanKind::SetAside,
            RepairKind::Convert => PlanKind::Convert,
        }
    }
}

impl Status for PlanKind {
    const ALL: &'static [Self] = &[
        PlanKind::Rename,
        PlanKind::Repack,
        PlanKind::Fill,
        PlanKind::Replace,
        PlanKind::SetAside,
        PlanKind::Convert,
    ];

    fn look(self) -> Look {
        match self {
            PlanKind::Rename => look(
                Glyph::PencilLine,
                Tone::Active,
                "Rename",
                "Writes the game under the DAT's names.",
            ),
            PlanKind::Repack => look(
                Glyph::Package,
                Tone::Active,
                "Repack",
                "Stores the game's ROMs together, as your storage policy says.",
            ),
            PlanKind::Fill => look(
                Glyph::CirclePlus,
                Tone::Good,
                "Fill",
                "Adds a missing game from an ingest folder.",
            ),
            PlanKind::Replace => look(
                Glyph::Replace,
                Tone::Review,
                "Replace",
                "Replaces a mismatched or unreadable entry, keeping the original.",
            ),
            PlanKind::SetAside => look(
                Glyph::Archive,
                Tone::Review,
                "Set aside",
                "Moves what no game needs to kept files.",
            ),
            PlanKind::Convert => look(
                Glyph::RefreshCw,
                Tone::Active,
                "Convert",
                "Rewrites the game in the format your storage policy now gives.",
            ),
        }
    }
}

/// The kind of a build plan item, or of a build's or cleanup's item, shown beside its row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BuildKind {
    Copy,
    Repack,
    Adopt,
    Unchanged,
    NoSource,
    Conflict,
    Unresolved,
    Blocked,
    Remove,
    SetAside,
    Acknowledge,
    Restore,
}

impl From<ItemKind> for BuildKind {
    fn from(kind: ItemKind) -> Self {
        match kind {
            ItemKind::Copy => BuildKind::Copy,
            ItemKind::Repack => BuildKind::Repack,
            ItemKind::Adopt => BuildKind::Adopt,
            ItemKind::Unchanged => BuildKind::Unchanged,
            ItemKind::NoSource => BuildKind::NoSource,
            ItemKind::Conflict => BuildKind::Conflict,
            ItemKind::Unresolved => BuildKind::Unresolved,
            ItemKind::Blocked => BuildKind::Blocked,
            ItemKind::Remove => BuildKind::Remove,
            ItemKind::KeepAside => BuildKind::SetAside,
            ItemKind::Acknowledge => BuildKind::Acknowledge,
            ItemKind::Restore => BuildKind::Restore,
        }
    }
}

impl Status for BuildKind {
    const ALL: &'static [Self] = &[
        BuildKind::Copy,
        BuildKind::Repack,
        BuildKind::Adopt,
        BuildKind::Unchanged,
        BuildKind::NoSource,
        BuildKind::Conflict,
        BuildKind::Unresolved,
        BuildKind::Blocked,
        BuildKind::Remove,
        BuildKind::SetAside,
        BuildKind::Acknowledge,
        BuildKind::Restore,
    ];

    fn look(self) -> Look {
        match self {
            BuildKind::Copy => look(
                Glyph::Copy,
                Tone::Active,
                "Copy",
                "Copies an archive that is already the game's TorrentZip.",
            ),
            BuildKind::Repack => look(
                Glyph::Package,
                Tone::Active,
                "Repack",
                "Writes a TorrentZip from the verified ROMs.",
            ),
            BuildKind::Adopt => look(
                Glyph::FolderCheck,
                Tone::Good,
                "Adopt",
                "Records the file already there as built, because its ROMs are the ones the DAT expects.",
            ),
            BuildKind::Unchanged => look(
                Glyph::Check,
                Tone::Neutral,
                "Unchanged",
                "The folder already holds this file, built from the same ROMs.",
            ),
            BuildKind::NoSource => look(
                Glyph::CircleOff,
                Tone::Review,
                "No source",
                "Skipped: no entry in the audit holds every ROM.",
            ),
            BuildKind::Conflict => look(
                Glyph::TriangleAlert,
                Tone::Review,
                "Conflict",
                "Skipped: a file Title Select didn't build, or one changed since, is at the target and is never overwritten.",
            ),
            BuildKind::Unresolved => look(
                Glyph::CircleQuestionMark,
                Tone::Review,
                "Unresolved",
                "Skipped: you chose to build the resolved groups only, so its target is left as it is.",
            ),
            BuildKind::Blocked => look(
                Glyph::CircleX,
                Tone::Failed,
                "Blocked",
                "Skipped: it can't be built as planned, such as for an invalid name.",
            ),
            BuildKind::Remove => look(
                Glyph::Trash2,
                Tone::Review,
                "Remove",
                "Removes a built file whose game is no longer selected.",
            ),
            BuildKind::SetAside => look(
                Glyph::Archive,
                Tone::Review,
                "Set aside",
                "Moves an entry Title Select didn't build to kept files.",
            ),
            BuildKind::Acknowledge => look(
                Glyph::Eye,
                Tone::Neutral,
                "Acknowledge",
                "Leaves an entry Title Select didn't build where it is.",
            ),
            BuildKind::Restore => look(
                Glyph::Undo2,
                Tone::Active,
                "Restore",
                "Puts a kept file back in the folder.",
            ),
        }
    }
}

/// Where an Ingest Folder item stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IngestStatus {
    Arriving,
    MatchesSourceSet,
    Duplicate,
    NoDatMatch,
    Moving,
    Done,
}

impl Status for IngestStatus {
    const ALL: &'static [Self] = &[
        IngestStatus::Arriving,
        IngestStatus::MatchesSourceSet,
        IngestStatus::Duplicate,
        IngestStatus::NoDatMatch,
        IngestStatus::Moving,
        IngestStatus::Done,
    ];

    fn look(self) -> Look {
        match self {
            IngestStatus::Arriving => look(
                Glyph::Download,
                Tone::Neutral,
                "Arriving",
                "It's still changing; it's assessed once it settles.",
            ),
            IngestStatus::MatchesSourceSet => look(
                Glyph::FolderCheck,
                Tone::Good,
                "Matches source set",
                "It holds a game a source set needs.",
            ),
            IngestStatus::Duplicate => look(
                Glyph::Copy,
                Tone::Neutral,
                "Duplicate",
                "Every source set it matches already holds it.",
            ),
            IngestStatus::NoDatMatch => look(
                Glyph::FileQuestionMark,
                Tone::Review,
                "No DAT match",
                "It matches no game in any DAT revision.",
            ),
            IngestStatus::Moving => look(
                Glyph::FolderInput,
                Tone::Active,
                "Moving",
                "A plan you approved is moving it.",
            ),
            IngestStatus::Done => look(
                Glyph::Check,
                Tone::Good,
                "Done",
                "A plan you approved moved it.",
            ),
        }
    }
}

/// Whether the store still keeps a DAT Revision's file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DatFileStatus {
    Kept,
    Deleted,
}

impl Status for DatFileStatus {
    const ALL: &'static [Self] = &[DatFileStatus::Kept, DatFileStatus::Deleted];

    fn look(self) -> Look {
        match self {
            DatFileStatus::Kept => look(
                Glyph::FileCheck,
                Tone::Good,
                "File kept",
                "Its DAT file is kept in the store.",
            ),
            DatFileStatus::Deleted => look(
                Glyph::FileX,
                Tone::Neutral,
                "File deleted",
                "Its DAT file was deleted from the store; import it again to restore it.",
            ),
        }
    }
}

impl Status for JobState {
    const ALL: &'static [Self] = &JobState::ALL;

    fn look(self) -> Look {
        match self {
            JobState::Queued => look(
                Glyph::Clock,
                Tone::Neutral,
                "Queued",
                "It starts when the jobs ahead of it finish.",
            ),
            JobState::Running => look(
                Glyph::LoaderCircle,
                Tone::Active,
                "Running",
                "It's running now.",
            ),
            JobState::Succeeded => {
                look(Glyph::CircleCheck, Tone::Good, "Succeeded", "It finished.")
            }
            JobState::Failed => look(
                Glyph::CircleX,
                Tone::Failed,
                "Failed",
                "It stopped with a problem. Its detail says why.",
            ),
            JobState::Interrupted => look(
                Glyph::CirclePause,
                Tone::Review,
                "Interrupted",
                "It stopped before finishing, because you cancelled it or the service restarted.",
            ),
        }
    }
}

/// A status as its icon alone, for a table cell: its label is the icon's tooltip and accessible name. In a
/// `parts::list` row, set `tab_stop` false: the row's detail pane names it too.
#[component]
pub fn StatusIcon<S: Status>(status: S, #[prop(default = true)] tab_stop: bool) -> impl IntoView {
    let look = status.look();
    view! {
        <span class=format!("status {}", look.tone.class())>
            <Icon glyph=look.glyph label=look.label tab_stop />
        </span>
    }
}

/// A status as its icon and label, for a detail pane or a summary: its hint is the icon's tooltip.
#[component]
pub fn StatusBadge<S: Status>(status: S) -> impl IntoView {
    let look = status.look();
    view! {
        <span class=format!("status {}", look.tone.class())>
            <Icon glyph=look.glyph label=look.hint />
            <span class="status-label">{look.label}</span>
        </span>
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    fn vocabulary<S: Status>() -> Vec<Look> {
        S::ALL.iter().map(|s| s.look()).collect()
    }

    fn every_vocabulary() -> Vec<(&'static str, Vec<Look>)> {
        vec![
            ("findings", vocabulary::<FindingStatus>()),
            ("game status", vocabulary::<GameStatus>()),
            ("source set rows", vocabulary::<SourceSetStatus>()),
            ("library folder rows", vocabulary::<LibraryStatus>()),
            ("plan kinds", vocabulary::<PlanKind>()),
            ("build plan kinds", vocabulary::<BuildKind>()),
            ("ingest items", vocabulary::<IngestStatus>()),
            ("jobs", vocabulary::<JobState>()),
            ("DAT files", vocabulary::<DatFileStatus>()),
        ]
    }

    #[test]
    fn labels_are_three_words_or_fewer_in_sentence_case() {
        for (name, looks) in every_vocabulary() {
            for look in looks {
                let words: Vec<&str> = look.label.split(' ').collect();
                assert!(
                    (1..=3).contains(&words.len()),
                    "{name}: {:?} is too long",
                    look.label
                );
                assert!(
                    words[0].starts_with(|c: char| c.is_ascii_uppercase()),
                    "{name}: {:?} starts in lowercase",
                    look.label
                );
                // Glossary terms are written in lowercase; only acronyms such as DAT keep capitals.
                for word in &words[1..] {
                    let acronym = word.chars().all(|c| c.is_ascii_uppercase());
                    assert!(
                        acronym || word.chars().all(|c| !c.is_ascii_uppercase()),
                        "{name}: {:?} capitalises {word}",
                        look.label
                    );
                }
            }
        }
    }

    #[test]
    fn statuses_in_one_vocabulary_differ_in_shape() {
        for (name, looks) in every_vocabulary() {
            let glyphs: HashSet<_> = looks.iter().map(|l| l.glyph).collect();
            let labels: HashSet<_> = looks.iter().map(|l| l.label).collect();
            assert_eq!(glyphs.len(), looks.len(), "{name} reuses a glyph");
            assert_eq!(labels.len(), looks.len(), "{name} reuses a label");
        }
    }

    #[test]
    fn hints_are_one_sentence_addressed_to_you() {
        for (name, looks) in every_vocabulary() {
            for look in looks {
                assert!(look.hint.ends_with('.'), "{name}: {:?}", look.hint);
                assert!(
                    !look.hint.contains("Operator") && !look.hint.contains("ADR"),
                    "{name}: {:?}",
                    look.hint
                );
            }
        }
    }

    #[test]
    fn every_build_item_kind_has_its_own_status() {
        let kinds: HashSet<BuildKind> = ItemKind::ALL.into_iter().map(BuildKind::from).collect();
        assert_eq!(kinds.len(), ItemKind::ALL.len());
        assert_eq!(kinds, BuildKind::ALL.iter().copied().collect());
        assert_eq!(BuildKind::from(ItemKind::NoSource), BuildKind::NoSource);
        assert_eq!(BuildKind::from(ItemKind::KeepAside), BuildKind::SetAside);
    }

    #[test]
    fn only_payload_evidence_makes_a_matched_entry_verified() {
        assert_eq!(
            FindingStatus::of(Finding::Matched, true),
            FindingStatus::Verified
        );
        assert_eq!(
            FindingStatus::of(Finding::Matched, false),
            FindingStatus::Matched
        );
        assert_eq!(
            FindingStatus::of(Finding::Unreadable, true),
            FindingStatus::Unreadable
        );
        for finding in Finding::ALL {
            assert_ne!(FindingStatus::from(finding), FindingStatus::Verified);
        }
    }

    #[test]
    fn a_source_set_shows_its_status_of_highest_precedence() {
        let audited = SourceSetFacts {
            audited: true,
            ..Default::default()
        };
        assert_eq!(
            SourceSetStatus::of(SourceSetFacts::default()),
            SourceSetStatus::NeverAudited
        );
        assert_eq!(SourceSetStatus::of(audited), SourceSetStatus::Verified);
        let stale = SourceSetFacts {
            stale: true,
            ..audited
        };
        assert_eq!(SourceSetStatus::of(stale), SourceSetStatus::Stale);
        let review = SourceSetFacts {
            needs_review: true,
            ..stale
        };
        assert_eq!(SourceSetStatus::of(review), SourceSetStatus::NeedsReview);
        let repair = SourceSetFacts {
            repair_open: true,
            ..review
        };
        assert_eq!(SourceSetStatus::of(repair), SourceSetStatus::RepairOpen);
        let running = SourceSetFacts {
            running: true,
            ..repair
        };
        assert_eq!(SourceSetStatus::of(running), SourceSetStatus::Running);
        // Its first audit running.
        let first = SourceSetFacts {
            running: true,
            ..Default::default()
        };
        assert_eq!(SourceSetStatus::of(first), SourceSetStatus::Running);
    }

    #[test]
    fn a_library_folder_shows_its_status_of_highest_precedence() {
        let built = LibraryFacts {
            built: true,
            ..Default::default()
        };
        assert_eq!(
            LibraryStatus::of(LibraryFacts::default()),
            LibraryStatus::NeverBuilt
        );
        assert_eq!(LibraryStatus::of(built), LibraryStatus::Current);
        let stale = LibraryFacts {
            stale: true,
            ..built
        };
        assert_eq!(LibraryStatus::of(stale), LibraryStatus::Stale);
        let incomplete = LibraryFacts {
            incomplete: true,
            ..stale
        };
        assert_eq!(LibraryStatus::of(incomplete), LibraryStatus::Incomplete);
        let drift = LibraryFacts {
            drift: true,
            ..incomplete
        };
        assert_eq!(LibraryStatus::of(drift), LibraryStatus::Drift);
        let plan = LibraryFacts {
            plan_open: true,
            ..drift
        };
        assert_eq!(LibraryStatus::of(plan), LibraryStatus::PlanOpen);
        let first = LibraryFacts {
            plan_open: true,
            ..Default::default()
        };
        assert_eq!(LibraryStatus::of(first), LibraryStatus::PlanOpen);
        let running = LibraryFacts {
            running: true,
            ..plan
        };
        assert_eq!(LibraryStatus::of(running), LibraryStatus::Running);
    }

    #[test]
    fn highest_picks_by_order_of_all() {
        assert_eq!(
            highest([JobState::Succeeded, JobState::Running, JobState::Failed]),
            Some(JobState::Running)
        );
        assert_eq!(highest::<JobState>([]), None);
    }

    #[test]
    fn every_repair_kind_has_a_plan_kind() {
        assert_eq!(PlanKind::from(RepairKind::Conflict), PlanKind::Rename);
        assert_eq!(PlanKind::from(RepairKind::KeepAside), PlanKind::SetAside);
        assert_eq!(PlanKind::from(RepairKind::Convert), PlanKind::Convert);
    }
}
