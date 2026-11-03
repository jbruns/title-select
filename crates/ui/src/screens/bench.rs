//! A synthetic Audit for the filtering benchmark in `crates/ui/bench/`: `/bench?entries=100000`, or with `&list=1`,
//! the same Entries in the shared list (`parts::list`).

use std::borrow::Cow;
use std::sync::Arc;

use leptos::prelude::*;
use leptos::tachys::view::any_view::AnyView;
use leptos_router::hooks::use_query_map;
use title_select_api::{
    ALGORITHMS, Entry, EntryRow, Evidence, EvidenceLevel, Finding, Game, GameRow, GameStatus,
    RomRef,
};

use crate::model::{AuditEntries, DerivedCounts};
use crate::parts::list::{Clip, Column, List, ListRow, Width};
use crate::screens::audit::EntriesPanel;

/// A synthetic Entry as a row of the shared list.
#[derive(Clone)]
struct BenchRow(Arc<EntryRow>);

// Bench rows never change once generated, so the shared row is its own identity.
impl PartialEq for BenchRow {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum BenchCol {
    Path,
    Finding,
}

impl ListRow for BenchRow {
    type Col = BenchCol;
    const COLUMNS: &'static [BenchCol] = &[BenchCol::Path, BenchCol::Finding];

    fn column(col: BenchCol) -> Column {
        match col {
            BenchCol::Path => Column::new(
                "path",
                "Path",
                Width::Flex {
                    min: 20.0,
                    share: 1,
                },
            )
            .sortable(),
            BenchCol::Finding => Column::new("finding", "Finding", Width::Fixed(10.0)),
        }
    }

    fn key(&self) -> String {
        self.0.seq.to_string()
    }

    fn title(&self) -> String {
        self.0.entry.path.clone()
    }

    fn search(&self) -> String {
        let e = &self.0.entry;
        format!("{} {}", e.path, e.member.as_deref().unwrap_or(""))
    }

    fn cell(&self, col: BenchCol) -> AnyView {
        match col {
            BenchCol::Path => view! { <Clip text=self.0.entry.path.clone() /> }.into_any(),
            BenchCol::Finding => format!("{:?}", self.0.entry.finding).into_any(),
        }
    }

    fn compare(&self, other: &Self, _col: BenchCol) -> std::cmp::Ordering {
        self.0.entry.path.cmp(&other.0.entry.path)
    }
}

const WORDS: [&str; 16] = [
    "Adventure",
    "Pitfall",
    "River",
    "Raid",
    "Space",
    "Invaders",
    "Combat",
    "Breakout",
    "Frogger",
    "Kaboom",
    "Enduro",
    "Freeway",
    "Boxing",
    "Tennis",
    "Skiing",
    "Bowling",
];
const REGIONS: [&str; 5] = ["USA", "Europe", "Japan", "World", "Brazil"];

/// Entries over half as many Games, with every Finding, both derived states and partial Evidence represented.
pub fn synthetic(entries: usize) -> AuditEntries {
    let mut seed: u64 = 0x5eed;
    let mut next = move || {
        seed = seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (seed >> 33) as usize
    };
    let game_count = entries.div_ceil(2).max(1);
    let games: Vec<GameRow> = (0..game_count)
        .map(|i| {
            let name = format!(
                "{} {} {i:05} ({})",
                WORDS[next() % WORDS.len()],
                WORDS[next() % WORDS.len()],
                REGIONS[next() % REGIONS.len()]
            );
            let status = [
                GameStatus::Complete,
                GameStatus::Complete,
                GameStatus::Incomplete,
                GameStatus::Missing,
            ][next() % 4];
            GameRow {
                seq: i as u64,
                game: Game {
                    claimed_roms: vec![],
                    missing_roms: if status == GameStatus::Complete {
                        vec![]
                    } else {
                        vec![format!("{name}.bin")]
                    },
                    name,
                    status,
                },
            }
        })
        .collect();
    let rows = (0..entries)
        .map(|i| {
            let game = &games[i / 2].game.name;
            let finding = [
                Finding::Matched,
                Finding::Matched,
                Finding::Matched,
                Finding::Matched,
                Finding::Misnamed,
                Finding::Conflicting,
                Finding::Mismatched,
                Finding::Unmatched,
                Finding::Unreadable,
            ][next() % 9];
            let algorithms = if next() % 3 == 0 {
                &ALGORITHMS[..2]
            } else {
                &ALGORITHMS[..]
            };
            let matched = !matches!(
                finding,
                Finding::Unmatched | Finding::Mismatched | Finding::Unreadable
            );
            EntryRow {
                seq: i as u64,
                entry: Entry {
                    path: format!("{game} [{}].zip", i % 2),
                    member: Some(format!("{game}.bin")),
                    finding,
                    roms: if matched {
                        vec![RomRef {
                            game: game.clone(),
                            name: format!("{game}.bin"),
                        }]
                    } else {
                        vec![]
                    },
                    named_rom: None,
                    evidence: (finding != Finding::Unreadable).then(|| Evidence {
                        level: EvidenceLevel::Payload,
                        read_at: None,
                        reused: None,
                        algorithms: algorithms.iter().map(|a| Cow::Borrowed(*a)).collect(),
                    }),
                    reason: (finding == Finding::Unreadable)
                        .then(|| "synthetic read error".to_string()),
                },
            }
        })
        .collect();
    AuditEntries::new(rows, &games)
}

#[component]
pub fn Bench() -> impl IntoView {
    let query = use_query_map();
    let n = query
        .read_untracked()
        .get("entries")
        .and_then(|n| n.parse().ok())
        .unwrap_or(100_000);
    let list = query.read_untracked().get("list").is_some();
    let entries = Arc::new(synthetic(n));
    let DerivedCounts {
        verified,
        needs_review,
    } = entries.counts();
    view! {
        <h1>"Filtering benchmark"</h1>
        <p class="lede">
            {format!(
                "{n} synthetic entries: {verified} verified, {needs_review} need review. Nothing here comes from the service.",
            )}
        </p>
        {if list {
            let rows: Arc<Vec<BenchRow>> =
                Arc::new(entries.rows.iter().cloned().map(|r| BenchRow(Arc::new(r))).collect());
            view! {
                <List
                    id="bench"
                    label="Synthetic entries"
                    rows=Signal::stored(rows)
                    empty="No entries."
                    detail=Callback::new(|row: BenchRow| row.0.entry.path.clone().into_any())
                />
            }
                .into_any()
        } else {
            view! { <EntriesPanel entries /> }.into_any()
        }}
    }
}
