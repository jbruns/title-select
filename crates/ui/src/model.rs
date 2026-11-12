//! The Audit screen's pure logic: the derived Verified and Needs Review states, and filtering and sorting of an
//! Audit's Entries and Game Statuses. It runs on every keystroke, so filtering works on precomputed keys.

use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap};

use title_select_api::{
    ALGORITHMS, AuditEvidence, AuditSummary, DatRevisionRecord, Entry, EntryRow, EvidenceLevel,
    Finding, Game, GameRow, GameStatus, GameWarning, HeaderRule, NamedHeaderRule, Reuse,
};

/// The glossary's name for a Finding.
pub fn finding_label(finding: Finding) -> &'static str {
    match finding {
        Finding::Matched => "Matched",
        Finding::Misnamed => "Misnamed",
        Finding::Conflicting => "Conflicting",
        Finding::Mismatched => "Mismatched",
        Finding::Unmatched => "Unmatched",
        Finding::Unreadable => "Unreadable",
    }
}

/// The glossary's name for a Game Status.
pub fn game_status_label(status: GameStatus) -> &'static str {
    match status {
        GameStatus::Complete => "Complete",
        GameStatus::Incomplete => "Incomplete",
        GameStatus::Missing => "Missing",
    }
}

/// A Game Status, saying when it rests, for some ROM, on Claim Evidence alone.
pub fn status_text(status: GameStatus, claimed: bool) -> String {
    if claimed {
        format!("{} (claim evidence only)", game_status_label(status))
    } else {
        game_status_label(status).to_string()
    }
}

/// A Game's status, saying when it rests, for some ROM, on Claim Evidence alone.
pub fn game_status_text(game: &Game) -> String {
    status_text(game.status, !game.claimed_roms.is_empty())
}

/// A Game's absent ROMs, each Available Elsewhere one naming the Games whose ZIP or folder holds it.
pub fn absent_roms(game: &Game) -> Vec<String> {
    game.missing_roms
        .iter()
        .map(
            |rom| match game.available_elsewhere.iter().find(|a| &a.rom == rom) {
                Some(a) => format!("{rom}: available in {}", a.games.join(" or ")),
                None => rom.clone(),
            },
        )
        .collect()
}

/// The states the glossary derives from an Entry's Finding and Evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Derived {
    /// Matched with Payload Evidence.
    Verified,
    /// Misnamed, Conflicting, Mismatched, Unmatched or Unreadable.
    NeedsReview,
}

impl Derived {
    pub fn label(self) -> &'static str {
        match self {
            Derived::Verified => "Verified",
            Derived::NeedsReview => "Needs review",
        }
    }
}

/// Verified or Needs Review; neither for a Matched Entry with only Claim Evidence.
pub fn derive(entry: &Entry) -> Option<Derived> {
    match entry.finding {
        Finding::Matched => entry
            .evidence
            .as_ref()
            .filter(|e| e.level == EvidenceLevel::Payload)
            .map(|_| Derived::Verified),
        _ => Some(Derived::NeedsReview),
    }
}

/// How many Entries are in each derived state.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DerivedCounts {
    pub verified: usize,
    pub needs_review: usize,
}

impl DerivedCounts {
    pub fn of<'a>(entries: impl IntoIterator<Item = &'a Entry>) -> DerivedCounts {
        let mut counts = DerivedCounts::default();
        for entry in entries {
            match derive(entry) {
                Some(Derived::Verified) => counts.verified += 1,
                Some(Derived::NeedsReview) => counts.needs_review += 1,
                None => {}
            }
        }
        counts
    }
}

/// The Needs Review count from an Audit's Finding counts alone, as for a pruned Audit whose Entries are gone. Verified
/// cannot be told from counts, since it depends on each Entry's Evidence.
pub fn needs_review_from_counts(findings: &BTreeMap<Finding, usize>) -> usize {
    findings
        .iter()
        .filter(|(f, _)| **f != Finding::Matched)
        .map(|(_, n)| n)
        .sum()
}

/// An Entry's name as the workbench shows it: its path, and its member within a ZIP archive.
pub fn entry_name(entry: &Entry) -> String {
    match &entry.member {
        Some(member) => format!("{} › {member}", entry.path),
        None => entry.path.clone(),
    }
}

/// An Entry's Evidence, such as `Payload: size, crc32, sha1`, or nothing for an Unreadable Entry.
pub fn evidence_text(entry: &Entry) -> String {
    match &entry.evidence {
        Some(evidence) => {
            let algorithms = evidence.algorithms.join(", ");
            match evidence.level {
                EvidenceLevel::Payload => format!("Payload: {algorithms}"),
                EvidenceLevel::Claim => format!("Claim: {algorithms} (not payload-verified)"),
            }
        }
        None => String::new(),
    }
}

/// When an Entry's payload was last read, and whether this Audit reused that read; nothing for Claim Evidence.
pub fn payload_read_text(entry: &Entry) -> Option<String> {
    let evidence = entry.evidence.as_ref()?;
    let read_at = when(&evidence.read_at?.to_string());
    Some(match evidence.reused {
        Some(Reuse::UnchangedFileHeuristic) => {
            format!("{read_at}, reused because the file looked unchanged (a heuristic)")
        }
        None => format!("{read_at}, by this audit"),
    })
}

/// The Header an Entry's payload starts with, which matching left out; nothing when it has none.
pub fn header_text(entry: &Entry) -> Option<String> {
    let header = entry.evidence.as_ref()?.header?;
    Some(format!(
        "{} bytes, left out when matching and kept in the file",
        header.bytes
    ))
}

/// What `warning` says of a Game, for a DAT Revision naming the Header Rule `rule`.
pub fn warning_text(warning: GameWarning, rule: Option<HeaderRule>) -> String {
    match warning {
        GameWarning::HeaderMissing => match rule {
            Some(HeaderRule::Nes) => "may not play: most NES emulators need the iNES header".into(),
            Some(HeaderRule::Atari7800) => {
                "may not play: most Atari 7800 emulators need its header".into()
            }
            _ => "may not play: most emulators need its header".into(),
        },
    }
}

/// How many of `audit`'s Games carry each Game Warning, and what it says of them; `None` when none do.
pub fn warning_counts(audit: &AuditSummary) -> Option<String> {
    let rule = audit.header_rule.as_ref().and_then(|r| r.rule);
    let said: Vec<String> = audit
        .warnings
        .iter()
        .map(|(warning, n)| {
            let games = if *n == 1 { "game" } else { "games" };
            format!("{n} {games} {}", warning_text(*warning, rule))
        })
        .collect();
    (!said.is_empty()).then(|| said.join("; "))
}

/// What a DAT Revision's Header Rule does to matching, in words.
pub fn header_rule_text(named: &NamedHeaderRule) -> String {
    match named.rule {
        Some(rule) => format!(
            "{} ({} bytes): left out when matching, never stripped",
            match rule {
                HeaderRule::Nes => "iNES header",
                HeaderRule::Fds => "FDS header",
                HeaderRule::Atari7800 => "Atari 7800 header",
                HeaderRule::Lynx => "Lynx header",
            },
            rule.size()
        ),
        None => format!(
            "{}: an unknown header rule, so no header is left out when matching",
            named.name
        ),
    }
}

/// How an Audit got its Entries' checksums, in words.
pub fn audit_evidence_label(evidence: AuditEvidence) -> &'static str {
    match evidence {
        AuditEvidence::Quick => {
            "Quick: claim evidence from ZIP directories without reading their payloads; other files read, \
             and earlier payload reads reused for files that looked unchanged (a heuristic)"
        }
        AuditEvidence::Incremental => {
            "Incremental: payload reads reused for files that looked unchanged (a heuristic)"
        }
        AuditEvidence::Full => "Full: every payload read",
    }
}

fn algorithm_bit(name: &str) -> u8 {
    ALGORITHMS
        .iter()
        .position(|a| *a == name)
        .map_or(0, |i| 1 << i)
}

fn status_bit(status: GameStatus) -> u8 {
    match status {
        GameStatus::Complete => 1,
        GameStatus::Incomplete => 2,
        GameStatus::Missing => 4,
    }
}

/// An Audit's Entries with the keys filtering and sorting use, computed once when they load.
#[derive(Debug, Default)]
pub struct AuditEntries {
    pub rows: Vec<EntryRow>,
    /// The lowercase text the filter searches: the name, member and the Games the Entry counts towards.
    search: Vec<String>,
    /// The lowercase name, for sorting.
    name: Vec<String>,
    /// The Game Statuses of the Games the Entry counts towards, as a bit set.
    statuses: Vec<u8>,
    /// The Game Status of the first Game the Entry counts towards, for sorting.
    primary_status: Vec<Option<GameStatus>>,
    /// The algorithms its Evidence compared, as a bit set over [`ALGORITHMS`].
    algorithms: Vec<u8>,
    derived: Vec<Option<Derived>>,
}

impl AuditEntries {
    /// Indexes the Entries, looking up each Game Status by the Game's name.
    pub fn new(rows: Vec<EntryRow>, games: &[GameRow]) -> AuditEntries {
        let status: HashMap<&str, GameStatus> = games
            .iter()
            .map(|g| (g.game.name.as_str(), g.game.status))
            .collect();
        let n = rows.len();
        let mut index = AuditEntries {
            search: Vec::with_capacity(n),
            name: Vec::with_capacity(n),
            statuses: Vec::with_capacity(n),
            primary_status: Vec::with_capacity(n),
            algorithms: Vec::with_capacity(n),
            derived: Vec::with_capacity(n),
            rows: Vec::new(),
        };
        for row in &rows {
            let entry = &row.entry;
            let name = entry_name(entry).to_lowercase();
            let mut search = name.clone();
            let mut statuses = 0;
            for rom in &entry.roms {
                search.push('\n');
                search.push_str(&rom.game.to_lowercase());
                if let Some(s) = status.get(rom.game.as_str()) {
                    statuses |= status_bit(*s);
                }
            }
            index.search.push(search);
            index.name.push(name);
            index.statuses.push(statuses);
            index.primary_status.push(
                entry
                    .roms
                    .first()
                    .and_then(|r| status.get(r.game.as_str()).copied()),
            );
            index.algorithms.push(
                entry
                    .evidence
                    .iter()
                    .flat_map(|e| &e.algorithms)
                    .fold(0, |bits, a| bits | algorithm_bit(a)),
            );
            index.derived.push(derive(entry));
        }
        index.rows = rows;
        index
    }

    pub fn len(&self) -> usize {
        self.rows.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    pub fn derived(&self, i: usize) -> Option<Derived> {
        self.derived[i]
    }

    pub fn primary_status(&self, i: usize) -> Option<GameStatus> {
        self.primary_status[i]
    }

    pub fn counts(&self) -> DerivedCounts {
        DerivedCounts::of(self.rows.iter().map(|r| &r.entry))
    }

    fn compare(&self, key: EntryColumn, a: usize, b: usize) -> Ordering {
        let (ea, eb) = (&self.rows[a].entry, &self.rows[b].entry);
        match key {
            EntryColumn::Name => self.name[a].cmp(&self.name[b]),
            EntryColumn::Finding => ea.finding.cmp(&eb.finding),
            EntryColumn::GameStatus => {
                cmp_none_last(self.primary_status[a], self.primary_status[b])
            }
            EntryColumn::Evidence => (self.algorithms[a].count_ones(), self.algorithms[a])
                .cmp(&(self.algorithms[b].count_ones(), self.algorithms[b])),
        }
    }

    /// The order of every Entry under `sort`; Entries that compare equal keep the report's order.
    pub fn sorted(&self, sort: Option<Sort<EntryColumn>>) -> Vec<u32> {
        let mut order: Vec<u32> = (0..self.len() as u32).collect();
        if let Some(sort) = sort {
            order.sort_by(|&a, &b| {
                let ord = self.compare(sort.column, a as usize, b as usize);
                if sort.descending { ord.reverse() } else { ord }
            });
        }
        order
    }

    /// The Entries of `order` that pass `filter`, in that order.
    pub fn filtered(&self, order: &[u32], filter: &EntryFilter) -> Vec<u32> {
        let text = filter.text.trim().to_lowercase();
        let algorithm = filter.algorithm.as_deref().map(algorithm_bit);
        order
            .iter()
            .copied()
            .filter(|&i| {
                let i = i as usize;
                filter
                    .finding
                    .is_none_or(|f| self.rows[i].entry.finding == f)
                    && filter.state.is_none_or(|s| self.derived[i] == Some(s))
                    && match filter.game_status {
                        None => true,
                        Some(StatusFilter::None) => self.statuses[i] == 0,
                        Some(StatusFilter::Is(s)) => self.statuses[i] & status_bit(s) != 0,
                    }
                    && algorithm.is_none_or(|bit| self.algorithms[i] & bit != 0)
                    && (text.is_empty() || self.search[i].contains(&text))
            })
            .collect()
    }
}

fn cmp_none_last<T: Ord>(a: Option<T>, b: Option<T>) -> Ordering {
    match (a, b) {
        (Some(a), Some(b)) => a.cmp(&b),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

/// A sortable column of the Entries table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum EntryColumn {
    Name,
    Finding,
    GameStatus,
    Evidence,
}

/// A sortable column of the Game Statuses table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum GameColumn {
    Name,
    Status,
    Absent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sort<C> {
    pub column: C,
    pub descending: bool,
}

/// Filters Entries by a Game Status, or by counting towards no Game.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusFilter {
    Is(GameStatus),
    None,
}

/// What the Operator narrowed the Entries to. Every part that is set must match.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EntryFilter {
    /// Case-insensitive text in the Entry's name or the names of the Games it counts towards.
    pub text: String,
    pub finding: Option<Finding>,
    pub state: Option<Derived>,
    pub game_status: Option<StatusFilter>,
    /// An algorithm the Evidence compared, such as `sha256`.
    pub algorithm: Option<String>,
}

/// What the Operator narrowed the Game Statuses to.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GameFilter {
    /// Case-insensitive text in the Game's name or its absent ROMs.
    pub text: String,
    pub status: Option<GameStatus>,
}

/// An Audit's Game Statuses with lowercase keys for filtering and sorting.
#[derive(Debug, Default)]
pub struct AuditGames {
    pub rows: Vec<GameRow>,
    name: Vec<String>,
    search: Vec<String>,
}

impl AuditGames {
    pub fn new(rows: Vec<GameRow>) -> AuditGames {
        let name: Vec<String> = rows.iter().map(|g| g.game.name.to_lowercase()).collect();
        let search = rows
            .iter()
            .zip(&name)
            .map(|(g, name)| {
                let mut s = name.clone();
                for rom in &g.game.missing_roms {
                    s.push('\n');
                    s.push_str(&rom.to_lowercase());
                }
                s
            })
            .collect();
        AuditGames { rows, name, search }
    }

    pub fn len(&self) -> usize {
        self.rows.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    pub fn sorted(&self, sort: Option<Sort<GameColumn>>) -> Vec<u32> {
        let mut order: Vec<u32> = (0..self.len() as u32).collect();
        if let Some(sort) = sort {
            order.sort_by(|&a, &b| {
                let (a, b) = (a as usize, b as usize);
                let (ga, gb) = (&self.rows[a].game, &self.rows[b].game);
                let ord = match sort.column {
                    GameColumn::Name => self.name[a].cmp(&self.name[b]),
                    GameColumn::Status => ga.status.cmp(&gb.status),
                    GameColumn::Absent => ga.missing_roms.len().cmp(&gb.missing_roms.len()),
                };
                if sort.descending { ord.reverse() } else { ord }
            });
        }
        order
    }

    pub fn filtered(&self, order: &[u32], filter: &GameFilter) -> Vec<u32> {
        let text = filter.text.trim().to_lowercase();
        order
            .iter()
            .copied()
            .filter(|&i| {
                let i = i as usize;
                filter.status.is_none_or(|s| self.rows[i].game.status == s)
                    && (text.is_empty() || self.search[i].contains(&text))
            })
            .collect()
    }
}

/// Parses the bulk Entries endpoint's body: one JSON Entry per line.
pub fn parse_ndjson(text: &str) -> Result<Vec<EntryRow>, String> {
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .enumerate()
        .map(|(n, line)| {
            serde_json::from_str(line).map_err(|e| format!("Entry on line {}: {e}", n + 1))
        })
        .collect()
}

/// The DAT Revisions an Audit of a Source Set can be started against: the one `POST /audits` defaults to, which is the
/// latest Audit's DAT Revision or else the newest, and those newer than it. `revisions` are the bound DAT's, oldest
/// first by header version.
pub fn audit_choices<'a>(
    history: &[AuditSummary],
    revisions: &'a [DatRevisionRecord],
) -> (Option<&'a DatRevisionRecord>, Vec<&'a DatRevisionRecord>) {
    let latest = history.first().and_then(|audit| {
        revisions
            .iter()
            .position(|r| r.sha256 == audit.dat_revision_sha256)
    });
    match latest.or(revisions.len().checked_sub(1)) {
        Some(i) => (Some(&revisions[i]), revisions[i + 1..].iter().collect()),
        None => (None, Vec::new()),
    }
}

/// The first 12 hex digits of a SHA-256, enough to tell DAT Revisions apart on screen.
pub fn short_sha(sha256: &str) -> &str {
    sha256.get(..12).unwrap_or(sha256)
}

/// A timestamp from the API, such as `2026-10-29T15:04:05.123Z`, to the second and in UTC.
pub fn when(timestamp: &str) -> String {
    match timestamp.get(..19) {
        Some(seconds) if timestamp.ends_with('Z') => {
            format!("{} UTC", seconds.replacen('T', " ", 1))
        }
        _ => timestamp.to_string(),
    }
}

/// How long ago something happened, `elapsed_ms` milliseconds before now, in the largest whole unit.
pub fn age(elapsed_ms: f64) -> String {
    let minutes = (elapsed_ms / 60_000.0).floor();
    let (n, unit) = if minutes < 1.0 {
        return "just now".to_string();
    } else if minutes < 60.0 {
        (minutes, "minute")
    } else if minutes < 24.0 * 60.0 {
        ((minutes / 60.0).floor(), "hour")
    } else {
        ((minutes / (24.0 * 60.0)).floor(), "day")
    };
    let plural = if n == 1.0 { "" } else { "s" };
    format!("{n} {unit}{plural} ago")
}

/// A UTC timestamp as `(seconds, nanoseconds)` text that sorts in time order, or `None` when it is not one.
fn instant(timestamp: &str) -> Option<(&str, String)> {
    let rest = timestamp.strip_suffix('Z')?;
    let seconds = rest.get(..19)?;
    let fraction = rest[19..].strip_prefix('.').unwrap_or("");
    fraction
        .bytes()
        .all(|b| b.is_ascii_digit())
        .then(|| (seconds, format!("{fraction:0<9}")))
}

/// Whether UTC timestamp `a` is before `b`; false when either is not a timestamp.
pub fn earlier(a: &str, b: &str) -> bool {
    matches!((instant(a), instant(b)), (Some(a), Some(b)) if a < b)
}

/// Whether a response is a plain-text file. The Service answers a path it has no file for with the workbench's
/// HTML page, so a missing licence text arrives as `200 text/html`.
pub fn is_plain_text(content_type: Option<&str>) -> bool {
    content_type.is_some_and(|t| {
        t.split(';')
            .next()
            .is_some_and(|mime| mime.trim().eq_ignore_ascii_case("text/plain"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::borrow::Cow;
    use title_select_api::{AvailableElsewhere, Evidence, RomRef};

    #[test]
    fn a_missing_header_warning_names_the_system_whose_emulators_need_it() {
        let warning = GameWarning::HeaderMissing;
        assert_eq!(
            warning_text(warning, Some(HeaderRule::Nes)),
            "may not play: most NES emulators need the iNES header"
        );
        assert_eq!(
            warning_text(warning, Some(HeaderRule::Atari7800)),
            "may not play: most Atari 7800 emulators need its header"
        );
        assert_eq!(
            warning_text(warning, None),
            "may not play: most emulators need its header"
        );
    }

    #[test]
    fn an_audit_counts_its_games_that_may_not_play_and_says_so_of_an_unknown_rule() {
        let mut summary = audit("a");
        assert_eq!(warning_counts(&summary), None);
        summary.header_rule = Some(NamedHeaderRule::new("No-Intro_NES.xml"));
        summary.warnings = BTreeMap::from([(GameWarning::HeaderMissing, 2)]);
        assert_eq!(
            warning_counts(&summary).as_deref(),
            Some("2 games may not play: most NES emulators need the iNES header")
        );
        summary.warnings = BTreeMap::from([(GameWarning::HeaderMissing, 1)]);
        assert_eq!(
            warning_counts(&summary).as_deref(),
            Some("1 game may not play: most NES emulators need the iNES header")
        );

        assert_eq!(
            header_rule_text(&NamedHeaderRule::new("No-Intro_NES.xml")),
            "iNES header (16 bytes): left out when matching, never stripped"
        );
        assert_eq!(
            header_rule_text(&NamedHeaderRule::new("No-Intro_SNES.xml")),
            "No-Intro_SNES.xml: an unknown header rule, so no header is left out when matching"
        );
    }

    fn entry(path: &str, finding: Finding, games: &[&str], algorithms: &[&'static str]) -> Entry {
        Entry {
            path: path.into(),
            member: None,
            finding,
            roms: games
                .iter()
                .map(|g| RomRef {
                    game: (*g).into(),
                    name: format!("{g}.a26"),
                })
                .collect(),
            named_rom: None,
            evidence: (finding != Finding::Unreadable).then(|| Evidence {
                level: EvidenceLevel::Payload,
                algorithms: algorithms.iter().map(|a| Cow::Borrowed(*a)).collect(),
                read_at: None,
                reused: None,
                header: None,
            }),
            reason: (finding == Finding::Unreadable).then(|| "permission denied".into()),
        }
    }

    fn rows(entries: Vec<Entry>) -> Vec<EntryRow> {
        entries
            .into_iter()
            .enumerate()
            .map(|(seq, entry)| EntryRow {
                seq: seq as u64,
                entry,
            })
            .collect()
    }

    fn game(name: &str, status: GameStatus) -> GameRow {
        GameRow {
            seq: 0,
            game: Game {
                name: name.into(),
                status,
                missing_roms: if status == GameStatus::Complete {
                    vec![]
                } else {
                    vec![format!("{name}.a26")]
                },
                claimed_roms: vec![],
                warnings: vec![],
                available_elsewhere: vec![],
            },
        }
    }

    #[test]
    fn a_game_status_resting_on_claims_says_so() {
        let mut claimed = game("Pong (USA)", GameStatus::Complete).game;
        claimed.claimed_roms = vec!["Pong (USA).a26".into()];

        assert_eq!(game_status_text(&claimed), "Complete (claim evidence only)");
        assert_eq!(
            game_status_text(&game("Combat (USA)", GameStatus::Complete).game),
            "Complete"
        );
    }

    #[test]
    fn an_absent_rom_available_elsewhere_names_the_games_holding_it() {
        let mut b = game("Game B (Europe)", GameStatus::Missing).game;
        b.missing_roms = vec!["Track 1.bin".into(), "Track 2.bin".into()];
        b.available_elsewhere = vec![AvailableElsewhere {
            rom: "Track 2.bin".into(),
            games: vec!["Game A (USA)".into(), "Game A (Japan)".into()],
        }];

        assert_eq!(
            absent_roms(&b),
            [
                "Track 1.bin",
                "Track 2.bin: available in Game A (USA) or Game A (Japan)"
            ]
        );
    }

    const FULL: &[&str] = &["size", "crc32", "md5", "sha1", "sha256"];

    fn fixture() -> AuditEntries {
        AuditEntries::new(
            rows(vec![
                entry("Pong (USA).zip", Finding::Matched, &["Pong (USA)"], FULL),
                entry(
                    "breakout.zip",
                    Finding::Misnamed,
                    &["Breakout (Europe)"],
                    FULL,
                ),
                entry(
                    "Combat (USA).zip",
                    Finding::Matched,
                    &["Combat (USA)"],
                    &["size", "crc32"],
                ),
                entry("junk.bin", Finding::Unmatched, &[], &["size", "crc32"]),
                entry("broken.zip", Finding::Unreadable, &[], &[]),
            ]),
            &[
                game("Pong (USA)", GameStatus::Complete),
                game("Breakout (Europe)", GameStatus::Complete),
                game("Combat (USA)", GameStatus::Incomplete),
                game("Adventure (USA)", GameStatus::Missing),
            ],
        )
    }

    fn names(index: &AuditEntries, order: &[u32]) -> Vec<String> {
        order
            .iter()
            .map(|&i| index.rows[i as usize].entry.path.clone())
            .collect()
    }

    #[test]
    fn verified_is_matched_with_payload_evidence_and_every_other_finding_needs_review() {
        let matched = entry("a", Finding::Matched, &["A"], FULL);
        assert_eq!(derive(&matched), Some(Derived::Verified));
        for finding in &Finding::ALL[1..] {
            assert_eq!(
                derive(&entry("a", *finding, &[], &["size"])),
                Some(Derived::NeedsReview),
                "{finding:?}"
            );
        }
        let no_evidence = Entry {
            evidence: None,
            ..matched
        };
        assert_eq!(
            derive(&no_evidence),
            None,
            "Matched without Payload Evidence is not Verified"
        );
    }

    #[test]
    fn counts_derive_verified_and_needs_review() {
        let index = fixture();
        assert_eq!(
            index.counts(),
            DerivedCounts {
                verified: 2,
                needs_review: 3
            }
        );
        let findings = BTreeMap::from([
            (Finding::Matched, 2),
            (Finding::Misnamed, 1),
            (Finding::Unmatched, 4),
        ]);
        assert_eq!(needs_review_from_counts(&findings), 5);
    }

    #[test]
    fn text_filter_is_case_insensitive_and_searches_game_names() {
        let index = fixture();
        let order = index.sorted(None);
        let by = |text: &str| {
            names(
                &index,
                &index.filtered(
                    &order,
                    &EntryFilter {
                        text: text.into(),
                        ..Default::default()
                    },
                ),
            )
        };
        assert_eq!(by("PONG"), ["Pong (USA).zip"]);
        assert_eq!(
            by("europe"),
            ["breakout.zip"],
            "found by the Game it counts towards"
        );
        assert_eq!(
            by("  "),
            names(&index, &order),
            "blank text matches everything"
        );
        assert!(by("nothing like this").is_empty());
    }

    #[test]
    fn filters_combine_finding_state_game_status_and_algorithm() {
        let index = fixture();
        let order = index.sorted(None);
        let by = |filter: EntryFilter| names(&index, &index.filtered(&order, &filter));
        assert_eq!(
            by(EntryFilter {
                finding: Some(Finding::Matched),
                ..Default::default()
            }),
            ["Pong (USA).zip", "Combat (USA).zip"]
        );
        assert_eq!(
            by(EntryFilter {
                state: Some(Derived::NeedsReview),
                ..Default::default()
            }),
            ["breakout.zip", "junk.bin", "broken.zip"]
        );
        assert_eq!(
            by(EntryFilter {
                game_status: Some(StatusFilter::Is(GameStatus::Incomplete)),
                ..Default::default()
            }),
            ["Combat (USA).zip"]
        );
        assert_eq!(
            by(EntryFilter {
                game_status: Some(StatusFilter::None),
                ..Default::default()
            }),
            ["junk.bin", "broken.zip"]
        );
        assert_eq!(
            by(EntryFilter {
                algorithm: Some("sha256".into()),
                ..Default::default()
            }),
            ["Pong (USA).zip", "breakout.zip"]
        );
        assert_eq!(
            by(EntryFilter {
                text: "usa".into(),
                finding: Some(Finding::Matched),
                algorithm: Some("sha256".into()),
                ..Default::default()
            }),
            ["Pong (USA).zip"]
        );
    }

    #[test]
    fn sorting_by_each_column_and_direction_keeps_report_order_for_ties() {
        let index = fixture();
        let sorted =
            |column, descending| names(&index, &index.sorted(Some(Sort { column, descending })));
        assert_eq!(
            sorted(EntryColumn::Name, false),
            [
                "breakout.zip",
                "broken.zip",
                "Combat (USA).zip",
                "junk.bin",
                "Pong (USA).zip"
            ],
            "case-insensitive"
        );
        assert_eq!(
            sorted(EntryColumn::Finding, false),
            [
                "Pong (USA).zip",
                "Combat (USA).zip",
                "breakout.zip",
                "junk.bin",
                "broken.zip"
            ]
        );
        assert_eq!(
            sorted(EntryColumn::Finding, true),
            [
                "broken.zip",
                "junk.bin",
                "breakout.zip",
                "Pong (USA).zip",
                "Combat (USA).zip"
            ]
        );
        assert_eq!(
            sorted(EntryColumn::GameStatus, false),
            [
                "Pong (USA).zip",
                "breakout.zip",
                "Combat (USA).zip",
                "junk.bin",
                "broken.zip"
            ],
            "Entries counting towards no Game go last"
        );
        assert_eq!(
            sorted(EntryColumn::Evidence, true)[..2],
            ["Pong (USA).zip", "breakout.zip"],
            "the most algorithms compared first"
        );
    }

    #[test]
    fn filtering_keeps_the_sorted_order() {
        let index = fixture();
        let order = index.sorted(Some(Sort {
            column: EntryColumn::Name,
            descending: true,
        }));
        let filter = EntryFilter {
            text: "usa".into(),
            ..Default::default()
        };
        assert_eq!(
            names(&index, &index.filtered(&order, &filter)),
            ["Pong (USA).zip", "Combat (USA).zip"]
        );
    }

    #[test]
    fn games_filter_by_status_and_absent_rom_and_sort() {
        let games = AuditGames::new(vec![
            game("Pong (USA)", GameStatus::Complete),
            game("Combat (USA)", GameStatus::Incomplete),
            game("Adventure (USA)", GameStatus::Missing),
        ]);
        let order = games.sorted(Some(Sort {
            column: GameColumn::Name,
            descending: false,
        }));
        let shown = |filter: GameFilter| -> Vec<String> {
            games
                .filtered(&order, &filter)
                .iter()
                .map(|&i| games.rows[i as usize].game.name.clone())
                .collect()
        };
        assert_eq!(
            shown(GameFilter::default()),
            ["Adventure (USA)", "Combat (USA)", "Pong (USA)"]
        );
        assert_eq!(
            shown(GameFilter {
                status: Some(GameStatus::Missing),
                ..Default::default()
            }),
            ["Adventure (USA)"]
        );
        assert_eq!(
            shown(GameFilter {
                text: "combat (usa).a26".into(),
                ..Default::default()
            }),
            ["Combat (USA)"],
            "found by an absent ROM"
        );
    }

    #[test]
    fn ndjson_parses_each_line_and_names_a_bad_one() {
        let text = concat!(
            r#"{"seq":0,"path":"a.zip","finding":"unmatched","evidence":{"level":"payload","algorithms":["size","crc32"]}}"#,
            "\n\n",
            r#"{"seq":1,"path":"b.zip","finding":"unreadable","reason":"denied"}"#,
            "\n"
        );
        let rows = parse_ndjson(text).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1].entry.reason.as_deref(), Some("denied"));
        assert!(parse_ndjson("{}\n").unwrap_err().contains("line 1"));
    }

    fn revision(sha: &str) -> DatRevisionRecord {
        DatRevisionRecord {
            sha256: sha.repeat(64),
            provider: "No-Intro".into(),
            name: "Atari - 2600".into(),
            version: sha.into(),
            file: format!("{sha}.dat"),
            imported_at: "2026-01-01T00:00:00Z".into(),
            source_url: None,
            blob_present: true,
        }
    }

    fn audit(sha: &str) -> AuditSummary {
        AuditSummary {
            id: 1,
            source_set: "Atari 2600".into(),
            started_at: "2026-01-01T00:00:00Z".into(),
            finished_at: "2026-01-01T00:01:00Z".into(),
            dat_revision_sha256: sha.repeat(64),
            dat_revision_version: sha.into(),
            findings: BTreeMap::new(),
            games: BTreeMap::new(),
            pruned: false,
            evidence: AuditEvidence::Incremental,
            claimed: Some(0),
            warnings: BTreeMap::new(),
            header_rule: None,
        }
    }

    #[test]
    fn a_headered_entry_says_how_big_its_header_is() {
        let mut headered = entry("a.nes", Finding::Matched, &["Zelda"], &["sha1"]);
        assert_eq!(header_text(&headered), None);
        headered.evidence.as_mut().unwrap().header = Some(title_select_api::Header { bytes: 16 });
        assert_eq!(
            header_text(&headered).as_deref(),
            Some("16 bytes, left out when matching and kept in the file")
        );
    }

    #[test]
    fn claim_evidence_never_reads_as_payload_verified() {
        let mut claimed = entry("a.zip", Finding::Matched, &["Pong"], &["size", "crc32"]);
        claimed.evidence.as_mut().unwrap().level = EvidenceLevel::Claim;

        assert_eq!(
            evidence_text(&claimed),
            "Claim: size, crc32 (not payload-verified)"
        );
        assert_eq!(payload_read_text(&claimed), None);
    }

    #[test]
    fn a_reused_payload_read_says_when_it_was_read_and_that_reuse_is_a_heuristic() {
        let mut read = entry("a.zip", Finding::Matched, &["Pong"], &["size", "crc32"]);
        read.evidence.as_mut().unwrap().read_at = Some("2026-01-02T03:04:05Z".parse().unwrap());
        let mut reused = read.clone();
        reused.evidence.as_mut().unwrap().reused = Some(Reuse::UnchangedFileHeuristic);

        assert_eq!(
            payload_read_text(&read).as_deref(),
            Some("2026-01-02 03:04:05 UTC, by this audit")
        );
        assert_eq!(
            payload_read_text(&reused).as_deref(),
            Some("2026-01-02 03:04:05 UTC, reused because the file looked unchanged (a heuristic)")
        );
    }

    #[test]
    fn audit_choices_default_to_the_latest_audits_revision_and_offer_newer_ones() {
        let revisions = [revision("a"), revision("b"), revision("c")];
        let versions = |(default, newer): (Option<&DatRevisionRecord>, Vec<&DatRevisionRecord>)| {
            (
                default.map(|r| r.version.clone()),
                newer.iter().map(|r| r.version.clone()).collect::<Vec<_>>(),
            )
        };
        assert_eq!(
            versions(audit_choices(&[audit("a"), audit("b")], &revisions)),
            (Some("a".into()), vec!["b".to_string(), "c".into()])
        );
        assert_eq!(
            versions(audit_choices(&[], &revisions)),
            (Some("c".into()), vec![]),
            "never audited: the newest import"
        );
        assert_eq!(versions(audit_choices(&[], &[])), (None, vec![]));
    }

    #[test]
    fn timestamps_show_to_the_second_in_utc() {
        assert_eq!(
            when("2026-10-29T15:04:05.123456Z"),
            "2026-10-29 15:04:05 UTC"
        );
        assert_eq!(when("2026-10-29T15:04:05Z"), "2026-10-29 15:04:05 UTC");
        assert_eq!(when("yesterday"), "yesterday");
        assert_eq!(short_sha(&"ab".repeat(32)), "abababababab");
    }

    #[test]
    fn ages_round_down_to_the_largest_whole_unit() {
        let minute = 60_000.0;
        assert_eq!(age(-5_000.0), "just now", "a clock slightly behind");
        assert_eq!(age(59_000.0), "just now");
        assert_eq!(age(minute), "1 minute ago");
        assert_eq!(age(59.0 * minute), "59 minutes ago");
        assert_eq!(age(60.0 * minute), "1 hour ago");
        assert_eq!(age(47.0 * 60.0 * minute), "1 day ago");
        assert_eq!(age(30.0 * 24.0 * 60.0 * minute), "30 days ago");
    }

    #[test]
    fn earlier_compares_instants_whatever_their_fractional_seconds() {
        assert!(earlier("2026-10-29T15:04:05Z", "2026-10-29T15:04:05.1Z"));
        assert!(!earlier(
            "2026-10-29T15:04:05.5Z",
            "2026-10-29T15:04:05.123456Z"
        ));
        assert!(earlier("2026-10-29T15:04:05.999Z", "2026-10-29T15:04:06Z"));
        assert!(!earlier("2026-10-29T15:04:05Z", "2026-10-29T15:04:05Z"));
        assert!(
            !earlier("yesterday", "2026-10-29T15:04:05Z"),
            "not a timestamp"
        );
    }

    #[test]
    fn a_licence_text_is_plain_text_and_the_workbench_page_is_not() {
        assert!(is_plain_text(Some("text/plain; charset=utf-8")));
        assert!(is_plain_text(Some("text/plain")));
        assert!(!is_plain_text(Some("text/html")));
        assert!(!is_plain_text(None));
    }
}
