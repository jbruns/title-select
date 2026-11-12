//! What the source set screens decide without rendering: a source set's row status, where its repair is, its page's
//! tabs, and one timeline of what happened to it.

use title_select_api::build::{BuildState, BuildSummary};
use title_select_api::ingest::{IngestAction, IngestPlan};
use title_select_api::repair::{RepairPlan, RepairStep};
use title_select_api::{
    AuditEvidence, AuditSummary, Finding, FollowedRevision, JobKind, JobState, OpenRepair,
    SourceSetStale, SourceSetSummary, StatWalk,
};

use crate::model::needs_review_from_counts;
use crate::parts::status::{RepairState, SourceSetFacts, SourceSetStatus, StaleCause};

/// What is true of a source set: `running` when a job is queued or running for it.
pub fn facts(summary: &SourceSetSummary, running: bool) -> SourceSetFacts {
    let latest = summary.latest.as_slice();
    SourceSetFacts {
        running,
        repair_open: summary.repair.is_some(),
        needs_review: latest
            .first()
            .is_some_and(|a| needs_review_from_counts(&a.findings) > 0),
        stale: !summary.set.stale.is_empty(),
        claims: latest.first().is_some_and(rests_on_claims),
        audited: !latest.is_empty(),
    }
}

/// Whether some of `audit`'s entries rest on Claim Evidence: those it counts, or, once it's pruned and they can't be
/// counted, whether it was a quick inventory.
pub fn rests_on_claims(audit: &AuditSummary) -> bool {
    match audit.claimed {
        Some(n) => n > 0,
        None => audit.evidence == AuditEvidence::Quick,
    }
}

/// What `audit`'s entries rest on: how many on payloads read, how many on claims, and how many couldn't be read.
pub fn evidence_coverage(audit: &AuditSummary) -> String {
    let total: usize = audit.findings.values().sum();
    let unreadable = audit
        .findings
        .get(&Finding::Unreadable)
        .copied()
        .unwrap_or(0);
    let entries = |n: usize| if n == 1 { "entry" } else { "entries" };
    let Some(claimed) = audit.claimed else {
        return format!(
            "Unknown: the audit is pruned, so its {total} {} are no longer kept",
            entries(total)
        );
    };
    let payload = total.saturating_sub(claimed + unreadable);
    let mut said = format!("{payload} of {total} {} on payloads read", entries(total));
    if claimed > 0 {
        said.push_str(&format!(
            "; {claimed} on claims, which never verify an entry"
        ));
    }
    if unreadable > 0 {
        said.push_str(&format!("; {unreadable} unreadable"));
    }
    said
}

/// The status that shows a source set's stale causes beside it.
pub const STALE: SourceSetStatus = SourceSetStatus::Stale;

pub fn status(summary: &SourceSetSummary, running: bool) -> SourceSetStatus {
    SourceSetStatus::of(facts(summary, running))
}

/// What a check counted, leaving out what it found none of: "2 changed, 1 added".
pub fn change_counts(changed: u64, added: u64, removed: u64) -> String {
    [(changed, "changed"), (added, "added"), (removed, "removed")]
        .into_iter()
        .filter(|(n, _)| *n > 0)
        .map(|(n, what)| format!("{n} {what}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Why a source set is stale: each cause, with a line that says what it found.
pub fn stale_causes(stale: &[SourceSetStale]) -> Vec<(StaleCause, String)> {
    stale
        .iter()
        .map(|s| match s {
            SourceSetStale::FilesChanged {
                changed,
                added,
                removed,
                ..
            } => (
                StaleCause::FilesChanged,
                format!("Files {}", change_counts(*changed, *added, *removed)),
            ),
            SourceSetStale::NewerRevision { version, .. } => (
                StaleCause::NewerRevision,
                format!("DAT revision {version} waits"),
            ),
        })
        .collect()
}

/// Where the source set's repair is, if one is under way: `proposing` while you wait for a plan, and `job_running`
/// while the open repair's job is queued or running.
pub fn repair_state(
    open: Option<&OpenRepair>,
    job_running: bool,
    proposing: bool,
) -> Option<RepairState> {
    if proposing {
        return Some(RepairState::Proposing);
    }
    let open = open?;
    Some(match (open.approved, job_running) {
        (true, true) => RepairState::Running,
        (true, false) => RepairState::Approved,
        (false, _) => RepairState::Open,
    })
}

/// A tab of a source set's page, named in the URL's `tab`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageTab {
    Overview,
    Games,
    Entries,
    Libraries,
    Additions,
    History,
    Kept,
    Repair,
}

impl PageTab {
    pub const ALL: [PageTab; 8] = [
        PageTab::Overview,
        PageTab::Games,
        PageTab::Entries,
        PageTab::Libraries,
        PageTab::Additions,
        PageTab::History,
        PageTab::Kept,
        PageTab::Repair,
    ];

    pub fn slug(self) -> &'static str {
        match self {
            PageTab::Overview => "overview",
            PageTab::Games => "games",
            PageTab::Entries => "entries",
            PageTab::Libraries => "libraries",
            PageTab::Additions => "additions",
            PageTab::History => "history",
            PageTab::Kept => "kept",
            PageTab::Repair => "repair",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            PageTab::Overview => "Overview",
            PageTab::Games => "Games",
            PageTab::Entries => "Entries",
            PageTab::Libraries => "Libraries",
            PageTab::Additions => "Additions",
            PageTab::History => "History",
            PageTab::Kept => "Kept files",
            PageTab::Repair => "Repair",
        }
    }

    /// The tab `tab` names; the overview when it names none.
    pub fn from_query(tab: Option<&str>) -> PageTab {
        PageTab::ALL
            .into_iter()
            .find(|t| Some(t.slug()) == tab)
            .unwrap_or(PageTab::Overview)
    }
}

/// The tabs a source set's page shows: Repair only while a repair is under way, or while you have it open.
pub fn tabs(repair: Option<RepairState>, open: PageTab) -> Vec<PageTab> {
    PageTab::ALL
        .into_iter()
        .filter(|t| *t != PageTab::Repair || repair.is_some() || open == PageTab::Repair)
        .collect()
}

/// What happened to a source set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Happened {
    Audit {
        id: i64,
        revision: String,
        needs_review: usize,
        pruned: bool,
    },
    Repair {
        id: i64,
        changes: usize,
        approved: bool,
        finished: bool,
    },
    /// Games a repair moved in from ingest folders.
    Ingest { repair: i64, games: usize },
    /// An approved ingest plan that changed the source set: a DAT import, or additions to its DAT's additions set.
    Ingested {
        id: i64,
        root: String,
        addition: bool,
        items: usize,
        finished: bool,
    },
    Build {
        id: i64,
        folder: i64,
        path: String,
        state: BuildState,
    },
    /// A check found files changed, added or removed since the audit it names.
    FilesChanged {
        walk: i64,
        audit: i64,
        changed: u64,
        added: u64,
        removed: u64,
    },
    /// A newer revision of the DAT it follows was imported.
    NewerRevision { sha256: String, version: String },
}

/// One moment of a source set's timeline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Moment {
    pub at: String,
    pub what: Happened,
}

impl Moment {
    /// Unique across the timeline.
    pub fn key(&self) -> String {
        match &self.what {
            Happened::Audit { id, .. } => format!("audit-{id}"),
            Happened::Repair { id, .. } => format!("repair-{id}"),
            Happened::Ingest { repair, .. } => format!("ingest-{repair}"),
            Happened::Ingested { id, .. } => format!("ingested-{id}"),
            Happened::Build { id, .. } => format!("build-{id}"),
            Happened::FilesChanged { walk, .. } => format!("walk-{walk}"),
            Happened::NewerRevision { sha256, .. } => format!("revision-{sha256}"),
        }
    }
}

/// One timeline, newest first, of a source set's audits, its repairs and the games they moved in from ingest folders,
/// the ingest plans that changed it, the builds of its library folders, each given with its folder's path, the checks
/// that found its files changed, and each newer revision of the DAT it follows that made it stale, whether or not it
/// still is.
pub fn timeline(
    audits: &[AuditSummary],
    repairs: &[RepairPlan],
    ingests: &[IngestPlan],
    builds: &[(String, BuildSummary)],
    walks: &[StatWalk],
    followed: &[FollowedRevision],
) -> Vec<Moment> {
    let mut moments: Vec<Moment> = audits
        .iter()
        .map(|a| Moment {
            at: a.finished_at.clone(),
            what: Happened::Audit {
                id: a.id,
                revision: a.dat_revision_version.clone(),
                needs_review: needs_review_from_counts(&a.findings),
                pruned: a.pruned,
            },
        })
        .collect();
    for p in repairs {
        let at = p
            .finished_at
            .clone()
            .or_else(|| p.approval.as_ref().map(|a| a.at.clone()))
            .unwrap_or_else(|| p.created_at.clone());
        let fills = p
            .content
            .steps
            .iter()
            .filter(|s| matches!(s.step, RepairStep::Fill { .. }))
            .count();
        if fills > 0 && p.approval.is_some() {
            moments.push(Moment {
                at: at.clone(),
                what: Happened::Ingest {
                    repair: p.id,
                    games: fills,
                },
            });
        }
        moments.push(Moment {
            at,
            what: Happened::Repair {
                id: p.id,
                changes: p.content.steps.len(),
                approved: p.approval.is_some(),
                finished: p.finished_at.is_some(),
            },
        });
    }
    moments.extend(ingests.iter().map(|p| {
        Moment {
            at: p
                .finished_at
                .clone()
                .or_else(|| p.approval.as_ref().map(|a| a.at.clone()))
                .unwrap_or_else(|| p.created_at.clone()),
            what: Happened::Ingested {
                id: p.id,
                root: p.content.root.clone(),
                addition: p.content.action == IngestAction::Addition,
                items: p.content.items.len(),
                finished: p.finished_at.is_some(),
            },
        }
    }));
    moments.extend(builds.iter().map(|(path, b)| {
        Moment {
            at: b
                .finished_at
                .clone()
                .unwrap_or_else(|| b.started_at.clone()),
            what: Happened::Build {
                id: b.id,
                folder: b.folder,
                path: path.clone(),
                state: b.state,
            },
        }
    }));
    moments.extend(walks.iter().map(|w| Moment {
        at: w.found_at.clone(),
        what: Happened::FilesChanged {
            walk: w.id,
            audit: w.audit,
            changed: w.changed,
            added: w.added,
            removed: w.removed,
        },
    }));
    moments.extend(followed.iter().map(|r| Moment {
        at: r.found_at.clone(),
        what: Happened::NewerRevision {
            sha256: r.sha256.clone(),
            version: r.version.clone(),
        },
    }));
    // Timestamps are ISO 8601 in UTC, so they sort as text.
    moments.sort_by(|a, b| b.at.cmp(&a.at));
    moments
}

/// The source sets running a job: those of the jobs given that are queued or running. A check for changes counts
/// only while it runs: one waits, at low priority, for every other job.
pub fn running<'a>(jobs: impl IntoIterator<Item = &'a title_select_api::Job>) -> Vec<String> {
    let mut names: Vec<String> = jobs
        .into_iter()
        .filter(|j| !j.state.is_final() && !j.source_set.is_empty())
        .filter(|j| !(j.kind == JobKind::StatWalk && j.state == JobState::Queued))
        .map(|j| j.source_set.clone())
        .collect();
    names.sort();
    names.dedup();
    names
}

/// What a source set's latest audit found missing, exported as a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MissingExport {
    Fixdat,
    HaveMiss,
}

/// The API path that downloads `export` for the source set whose escaped name is `set`, limited to the winners of
/// `selection` when there is one.
pub fn missing_export_path(set: &str, export: MissingExport, selection: Option<i64>) -> String {
    let file = match export {
        MissingExport::Fixdat => "fixdat",
        MissingExport::HaveMiss => "have-miss.csv",
    };
    match selection {
        Some(id) => format!("/source-sets/{set}/{file}?selection={id}"),
        None => format!("/source-sets/{set}/{file}"),
    }
}

/// The selections an export can be limited to: those of the latest audit's DAT revision that still hold their rows.
pub fn export_selections<'a>(
    latest: &AuditSummary,
    selections: &'a [title_select_api::SelectionRecord],
) -> Vec<&'a title_select_api::SelectionRecord> {
    selections
        .iter()
        .filter(|s| !s.pruned && s.dat_revision_sha256 == latest.dat_revision_sha256)
        .collect()
}

/// How a selection is offered in the export's choice.
pub fn selection_option(selection: &title_select_api::SelectionRecord) -> String {
    format!(
        "Selection #{}: {} v{}",
        selection.id, selection.profile.name, selection.profile.version
    )
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use title_select_api::{Finding, SourceSetRecord};

    use super::*;

    fn audit(id: i64, at: &str, sha: &str, review: usize) -> AuditSummary {
        AuditSummary {
            id,
            source_set: "GB".into(),
            started_at: at.into(),
            finished_at: at.into(),
            dat_revision_sha256: sha.into(),
            dat_revision_version: sha.into(),
            findings: BTreeMap::from([(Finding::Unmatched, review)]),
            games: BTreeMap::new(),
            pruned: false,
            evidence: Default::default(),
            claimed: Some(0),
        }
    }

    fn summary(latest: Option<AuditSummary>, repair: Option<OpenRepair>) -> SourceSetSummary {
        SourceSetSummary {
            set: SourceSetRecord {
                name: "GB".into(),
                root: "sets".into(),
                path: "GB".into(),
                dat: "No-Intro/Nintendo - Game Boy".into(),
                added_at: "2026-01-01T00:00:00Z".into(),
                last_verified_against: None,
                follow: false,
                stale: Vec::new(),
            },
            latest,
            repair,
        }
    }

    fn open(approved: bool) -> OpenRepair {
        OpenRepair {
            id: 3,
            audit: 1,
            approved,
            job: approved.then_some(9),
        }
    }

    #[test]
    fn a_source_sets_row_status_follows_its_summary() {
        assert_eq!(
            status(&summary(None, None), false),
            SourceSetStatus::NeverAudited
        );
        assert_eq!(
            status(&summary(Some(audit(1, "t", "b", 0)), None), false),
            SourceSetStatus::Verified
        );
        let mut stale = summary(Some(audit(1, "t", "b", 0)), None);
        stale.set.stale = vec![newer("c")];
        assert_eq!(
            status(&stale, false),
            SourceSetStatus::Stale,
            "a newer revision of the DAT it follows waits"
        );
        stale.latest = None;
        assert_eq!(
            status(&stale, false),
            SourceSetStatus::NeverAudited,
            "never audited, never stale"
        );
        let claimed = AuditSummary {
            claimed: Some(3),
            ..audit(1, "t", "b", 0)
        };
        assert_eq!(
            status(&summary(Some(claimed), None), false),
            SourceSetStatus::OnClaims,
            "claims never make it verified"
        );
        let mut covered = audit(1, "t", "b", 1);
        covered.findings.insert(Finding::Matched, 9);
        covered.findings.insert(Finding::Unreadable, 2);
        covered.claimed = Some(3);
        assert_eq!(
            evidence_coverage(&covered),
            "7 of 12 entries on payloads read; 3 on claims, which never verify an entry; 2 unreadable"
        );
        let pruned_quick = AuditSummary {
            claimed: None,
            evidence: AuditEvidence::Quick,
            ..audit(1, "t", "b", 0)
        };
        assert_eq!(
            status(&summary(Some(pruned_quick), None), false),
            SourceSetStatus::OnClaims,
            "a quick inventory whose entries are gone can't be told verified"
        );
        assert_eq!(
            status(&summary(Some(audit(1, "t", "b", 2)), None), false),
            SourceSetStatus::NeedsReview
        );
        assert_eq!(
            status(
                &summary(Some(audit(1, "t", "b", 2)), Some(open(false))),
                false
            ),
            SourceSetStatus::RepairOpen
        );
        assert_eq!(
            status(
                &summary(Some(audit(1, "t", "b", 2)), Some(open(true))),
                true
            ),
            SourceSetStatus::Running
        );
    }

    #[test]
    fn the_repair_tab_shows_while_a_repair_is_under_way_or_open_in_the_url() {
        assert_eq!(repair_state(None, false, false), None);
        assert_eq!(
            repair_state(None, false, true),
            Some(RepairState::Proposing)
        );
        assert_eq!(
            repair_state(Some(&open(false)), false, false),
            Some(RepairState::Open)
        );
        assert_eq!(
            repair_state(Some(&open(true)), false, false),
            Some(RepairState::Approved)
        );
        assert_eq!(
            repair_state(Some(&open(true)), true, false),
            Some(RepairState::Running)
        );
        assert!(!tabs(None, PageTab::Overview).contains(&PageTab::Repair));
        assert!(tabs(None, PageTab::Repair).contains(&PageTab::Repair));
        assert!(tabs(Some(RepairState::Open), PageTab::Games).contains(&PageTab::Repair));
        assert_eq!(PageTab::from_query(Some("kept")), PageTab::Kept);
        assert_eq!(PageTab::from_query(Some("nope")), PageTab::Overview);
        assert_eq!(PageTab::from_query(None), PageTab::Overview);
    }

    #[test]
    fn the_timeline_puts_audits_repairs_ingests_and_builds_newest_first() {
        let repair: RepairPlan = serde_json::from_value(serde_json::json!({
            "id": 4, "digest": "d", "created_at": "2026-01-02T00:00:00Z",
            "content": {
                "source_set": "GB", "set_id": 1, "root": "sets", "path": "GB", "audit": 1,
                "dat_revision": "r", "generation": 1, "creates_marker": false,
                "folder": {"dev": 1, "ino": 2}, "items": [], "steps": [], "fingerprint": []
            },
            "approval": {"digest": "d", "actor": "Operator", "at": "2026-01-03T00:00:00Z"},
            "finished_at": "2026-01-04T00:00:00Z",
        }))
        .unwrap();
        let build: BuildSummary = serde_json::from_value(serde_json::json!({
            "id": 7, "folder": 2, "plan": 5, "job": 8, "state": "succeeded", "generation": 1,
            "selection": 1, "audit": 1, "dat_revision": "r", "started_at": "2026-01-05T00:00:00Z",
            "finished_at": "2026-01-06T00:00:00Z", "applied": 1, "failed": 0, "pending": 0,
            "planned": {"copy": 1, "repack": 0, "unchanged": 0, "no_source": 0, "conflict": 0, "blocked": 0}
        }))
        .unwrap();
        let ingest: IngestPlan = serde_json::from_value(serde_json::json!({
            "id": 9, "digest": "d", "created_at": "2026-01-02T12:00:00Z",
            "content": {
                "ingest_folder": 1, "root": "drop", "action": "dat_import", "generation": 1,
                "items": [], "fingerprint": []
            },
            "approval": {"digest": "d", "actor": "Operator", "at": "2026-01-02T13:00:00Z"},
        }))
        .unwrap();
        let walk = StatWalk {
            id: 11,
            source_set: "GB".into(),
            audit: 1,
            found_at: "2026-01-07T00:00:00Z".into(),
            checked_at: "2026-01-09T00:00:00Z".into(),
            changed: 1,
            added: 0,
            removed: 2,
        };
        let moments = timeline(
            &[audit(1, "2026-01-01T00:00:00Z", "a", 0)],
            &[repair],
            &[ingest],
            &[("GB".into(), build)],
            &[walk],
            // The older made it stale before its latest audit, which cleared it; it stays in the timeline.
            &[
                followed("c", "2026-01-08T00:00:00Z"),
                followed("b", "2025-12-31T00:00:00Z"),
            ],
        );
        let keys: Vec<String> = moments.iter().map(Moment::key).collect();
        assert_eq!(
            keys,
            [
                "revision-c",
                "walk-11",
                "build-7",
                "repair-4",
                "ingested-9",
                "audit-1",
                "revision-b"
            ]
        );
        assert_eq!(
            moments[1].at, "2026-01-07T00:00:00Z",
            "a check is when it first found the changes"
        );
        assert_eq!(
            moments[4].at, "2026-01-02T13:00:00Z",
            "an unfinished ingest plan is when it was approved"
        );
        assert_eq!(
            moments[3].at, "2026-01-04T00:00:00Z",
            "a finished repair is when it finished"
        );
    }

    #[test]
    fn a_source_set_is_running_while_a_job_for_it_is_queued_or_running() {
        let job =
            |name: &str, state: &str| -> title_select_api::Job { job_of("audit", name, state) };
        let jobs = [
            job("GB", "running"),
            job("GBA", "succeeded"),
            job("GB", "queued"),
            job("", "running"),
            job_of("stat_walk", "GBC", "queued"),
            job_of("stat_walk", "NES", "running"),
        ];
        assert_eq!(
            running(&jobs),
            ["GB", "NES"],
            "a check waiting isn't running"
        );
    }

    fn job_of(kind: &str, name: &str, state: &str) -> title_select_api::Job {
        {
            serde_json::from_value(serde_json::json!({
                "id": 1, "kind": kind, "state": state, "source_set": name, "dat_revision": "r", "actor": "Operator",
                "attempt": 1, "requeued_from": null, "audit": null, "created_at": "t",
                "workers": {"readers": 1, "hashers": 1}, "version": 1,
            }))
            .unwrap()
        }
    }

    fn followed(sha: &str, found_at: &str) -> FollowedRevision {
        FollowedRevision {
            source_set: "GB".into(),
            sha256: sha.into(),
            version: sha.into(),
            imported_at: found_at.into(),
            found_at: found_at.into(),
        }
    }

    fn newer(sha: &str) -> SourceSetStale {
        SourceSetStale::NewerRevision {
            sha256: sha.into(),
            version: sha.into(),
            imported_at: "2026-01-08T00:00:00Z".into(),
        }
    }

    #[test]
    fn a_stale_source_set_says_each_cause_with_what_it_found() {
        let files = SourceSetStale::FilesChanged {
            walk: 1,
            found_at: "t".into(),
            changed: 2,
            added: 0,
            removed: 1,
        };
        assert_eq!(
            stale_causes(&[files, newer("20260201")]),
            [
                (
                    StaleCause::FilesChanged,
                    "Files 2 changed, 1 removed".into()
                ),
                (
                    StaleCause::NewerRevision,
                    "DAT revision 20260201 waits".into()
                ),
            ]
        );
        assert_eq!(change_counts(0, 3, 0), "3 added");
    }

    fn selection(id: i64, sha: &str, pruned: bool) -> title_select_api::SelectionRecord {
        title_select_api::SelectionRecord {
            id,
            source_set: "GB".into(),
            dat_revision_sha256: sha.into(),
            dat_revision_version: sha.into(),
            profile: title_select_api::ProfileVersionRef {
                name: "Mine".into(),
                version: 3,
            },
            latest_profile_version: 3,
            job: None,
            system: "Nintendo - Game Boy".into(),
            system_settings: false,
            pins: Default::default(),
            summary: Default::default(),
            created_at: "2026-01-02T00:00:00Z".into(),
            pruned,
            library_folders: Vec::new(),
        }
    }

    #[test]
    fn what_is_missing_exports_by_the_latest_audit_limited_to_a_selection_of_its_revision() {
        let latest = audit(4, "2026-01-03T00:00:00Z", "aaa", 0);
        let selections = [
            selection(9, "aaa", false),
            selection(8, "bbb", false),
            selection(7, "aaa", true),
            selection(6, "aaa", false),
        ];

        let offered: Vec<i64> = export_selections(&latest, &selections)
            .into_iter()
            .map(|s| s.id)
            .collect();

        assert_eq!(offered, [9, 6]);
        assert_eq!(selection_option(&selections[0]), "Selection #9: Mine v3");
        assert_eq!(
            missing_export_path("GB%20Color", MissingExport::Fixdat, None),
            "/source-sets/GB%20Color/fixdat"
        );
        assert_eq!(
            missing_export_path("GB", MissingExport::HaveMiss, Some(9)),
            "/source-sets/GB/have-miss.csv?selection=9"
        );
    }
}
