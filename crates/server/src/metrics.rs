//! `/metrics`: Prometheus text exposition, read from the store at scrape time. Labels are Job kinds, outcomes, Source
//! Set names, providers, systems and Library Folder ids: never a path or a Game's name.

use std::fmt::Write;

use title_select_store::observe::Metrics;

/// The content type of the text exposition format.
pub const CONTENT_TYPE: &str = "text/plain; version=0.0.4; charset=utf-8";

/// Every Job kind, so that each counter is present from the start.
const KINDS: [&str; 8] = [
    "audit",
    "selection",
    "build",
    "check",
    "ingest_scan",
    "repair",
    "additions",
    "stat_walk",
];
/// Every final Job state.
const OUTCOMES: [&str; 3] = ["succeeded", "failed", "interrupted"];

/// A label value, escaped.
fn label(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
}

fn header(out: &mut String, name: &str, kind: &str, help: &str) {
    let _ = writeln!(out, "# HELP {name} {help}");
    let _ = writeln!(out, "# TYPE {name} {kind}");
}

/// `metrics` in the text exposition format.
pub fn render(metrics: &Metrics) -> String {
    let mut out = String::new();
    header(
        &mut out,
        "title_select_build_info",
        "gauge",
        "The running version of Title Select.",
    );
    let _ = writeln!(
        out,
        "title_select_build_info{{version=\"{}\"}} 1",
        label(env!("CARGO_PKG_VERSION"))
    );

    header(
        &mut out,
        "title_select_jobs_total",
        "counter",
        "Jobs that ended, by kind and outcome.",
    );
    let mut kinds: Vec<String> = KINDS.iter().map(|k| k.to_string()).collect();
    for (kind, _) in metrics.jobs_ended.keys() {
        if !kinds.contains(kind) {
            kinds.push(kind.clone());
        }
    }
    for kind in &kinds {
        for outcome in OUTCOMES {
            let count = metrics
                .jobs_ended
                .get(&(kind.clone(), outcome.to_owned()))
                .copied()
                .unwrap_or(0);
            let _ = writeln!(
                out,
                "title_select_jobs_total{{kind=\"{}\",outcome=\"{outcome}\"}} {count}",
                label(kind)
            );
        }
    }

    header(
        &mut out,
        "title_select_jobs_running",
        "gauge",
        "Jobs running now.",
    );
    let _ = writeln!(out, "title_select_jobs_running {}", metrics.jobs_running);

    header(
        &mut out,
        "title_select_source_sets_stale",
        "gauge",
        "Source sets that are stale.",
    );
    let stale = metrics.source_sets.iter().filter(|s| s.stale).count();
    let _ = writeln!(out, "title_select_source_sets_stale {stale}");

    header(
        &mut out,
        "title_select_source_set_stale",
        "gauge",
        "1 if the source set is stale, else 0.",
    );
    for set in &metrics.source_sets {
        let _ = writeln!(
            out,
            "title_select_source_set_stale{{source_set=\"{}\"}} {}",
            label(&set.name),
            u8::from(set.stale)
        );
    }

    header(
        &mut out,
        "title_select_source_set_last_audit_timestamp_seconds",
        "gauge",
        "When the source set's latest audit finished, in seconds since the Unix epoch.",
    );
    for set in &metrics.source_sets {
        if let Some(at) = set.last_audit {
            let _ = writeln!(
                out,
                "title_select_source_set_last_audit_timestamp_seconds{{source_set=\"{}\"}} {at}",
                label(&set.name)
            );
        }
    }

    header(
        &mut out,
        "title_select_fetch_last_timestamp_seconds",
        "gauge",
        "When the latest fetch of a provider's system finished, in seconds since the Unix epoch.",
    );
    for fetch in &metrics.fetches {
        let _ = writeln!(
            out,
            "title_select_fetch_last_timestamp_seconds{{provider=\"{}\",system=\"{}\"}} {}",
            label(&fetch.provider),
            label(&fetch.system),
            fetch.finished_at
        );
    }

    header(
        &mut out,
        "title_select_fetch_last_result",
        "gauge",
        "1 for the outcome of the latest fetch of a provider's system: added, unchanged or failed.",
    );
    for fetch in &metrics.fetches {
        let _ = writeln!(
            out,
            "title_select_fetch_last_result{{provider=\"{}\",system=\"{}\",outcome=\"{}\"}} 1",
            label(&fetch.provider),
            label(&fetch.system),
            label(&fetch.outcome)
        );
    }

    header(
        &mut out,
        "title_select_library_folder_drift",
        "gauge",
        "Entries Title Select did not build that the latest check of a play library folder found.",
    );
    for folder in &metrics.library_folders {
        let _ = writeln!(
            out,
            "title_select_library_folder_drift{{library_folder=\"{}\",root=\"{}\",source_set=\"{}\"}} {}",
            folder.id,
            label(&folder.root),
            label(&folder.source_set),
            folder.drift
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn label_values_are_escaped() {
        assert_eq!(label("a\\b\"c\nd"), "a\\\\b\\\"c\\nd");
    }
}
