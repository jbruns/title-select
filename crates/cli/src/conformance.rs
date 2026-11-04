//! `title-select conformance`: the built-in selection engine against the bundled one, DAT by DAT.

use std::path::{Path, PathBuf};

use serde::Serialize;
use title_select_api::selection::SelectionProfile;
use title_select_selection::{Conformance, Engine, RunOptions, Verdict};

/// One DAT's comparison, as `--json` prints it.
#[derive(Serialize)]
struct Report {
    dat: PathBuf,
    verdict: Verdict,
    conforms: bool,
    groups: usize,
    units: usize,
    set_orders: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    bundled_error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    builtin_error: Option<String>,
    differences: Vec<Difference>,
}

#[derive(Serialize)]
struct Difference {
    unit: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    group: Option<String>,
    order_sensitive: bool,
    /// The set order whose rerun of the bundled engine gave the built-in engine's result for this difference and
    /// everything that depends on it.
    #[serde(skip_serializing_if = "Option::is_none")]
    witness: Option<u64>,
    bundled: Option<String>,
    builtin: Option<String>,
}

impl Report {
    fn new(dat: &Path, c: &Conformance) -> Report {
        Report {
            dat: dat.to_path_buf(),
            verdict: c.verdict(),
            conforms: c.conforms(),
            groups: c.comparison.as_ref().map_or(0, |c| c.groups),
            units: c.units,
            set_orders: c.set_orders,
            bundled_error: c.failures[0].clone(),
            builtin_error: c.failures[1].clone(),
            differences: c
                .differing
                .iter()
                .map(|(unit, [bundled, builtin])| Difference {
                    unit: unit.clone(),
                    group: Conformance::group_of(unit).map(str::to_owned),
                    order_sensitive: c.witness(unit).is_some(),
                    witness: c.witness(unit),
                    bundled: bundled.clone(),
                    builtin: builtin.clone(),
                })
                .collect(),
        }
    }
}

/// Compares the engines on each DAT, printing each DAT's result as it comes, then a summary. Fails when any DAT
/// isn't the same or the same apart from order-sensitive differences.
pub fn run(
    engine: &Engine,
    dats: &[PathBuf],
    profile: &SelectionProfile,
    options: &RunOptions,
    orders: u32,
    json: bool,
) -> Result<(), String> {
    let mut reports = Vec::new();
    for dat in dats {
        let conformance = engine
            .conformance(dat, profile, options, orders)
            .map_err(|e| format!("{}: {e}", dat.display()))?;
        let report = Report::new(dat, &conformance);
        if !json {
            print!("{}", text(&report));
        }
        reports.push(report);
    }
    let differ = reports.iter().filter(|r| !r.conforms).count();
    if json {
        super::write_report(&reports, None)?;
    } else {
        let with = |verdict| reports.iter().filter(|r| r.verdict == verdict).count();
        println!(
            "\n{} compared: {} the same, {} the same apart from order-sensitive differences, {} different, {} \
             inconclusive, {} unsupported.",
            count(reports.len(), "DAT"),
            with(Verdict::Same),
            with(Verdict::OrderSensitive),
            with(Verdict::Different),
            with(Verdict::Inconclusive),
            with(Verdict::Unsupported),
        );
    }
    match differ {
        0 => Ok(()),
        n => Err(format!(
            "the engines don't conform on {} of {}",
            n,
            count(reports.len(), "DAT")
        )),
    }
}

fn count(n: usize, what: &str) -> String {
    format!("{n} {what}{}", if n == 1 { "" } else { "s" })
}

/// One DAT's result as text: a headline, then each difference no rerun explains, with both engines' versions.
fn text(r: &Report) -> String {
    let mut out = format!("{}\n", r.dat.display());
    match (&r.bundled_error, &r.builtin_error) {
        (_, Some(b)) if r.verdict == Verdict::Unsupported => {
            out += &format!("  UNSUPPORTED: {b}\n");
            return out;
        }
        (Some(a), Some(b)) if r.verdict == Verdict::Same => {
            out += &format!(
                "  Both engines failed for the same reason, so they agree.\n    bundled: {a}\n    built-in: {b}\n"
            );
            return out;
        }
        (Some(a), Some(b)) => {
            out += &format!(
                "  INCONCLUSIVE: both engines failed, for different or unrecognised reasons.\n    bundled: {a}\n    \
                 built-in: {b}\n"
            );
            return out;
        }
        (Some(a), None) => {
            out += &format!("  DIFFERENT: only the bundled engine failed: {a}\n");
            return out;
        }
        (None, Some(b)) => {
            out += &format!("  DIFFERENT: only the built-in engine failed: {b}\n");
            return out;
        }
        (None, None) => {}
    }
    let scope = format!(
        "{} across {}",
        count(r.units, "check"),
        count(r.groups, "group")
    );
    let order_sensitive = r.differences.iter().filter(|d| d.order_sensitive).count();
    let real: Vec<&Difference> = r
        .differences
        .iter()
        .filter(|d| !d.order_sensitive)
        .collect();
    if r.differences.is_empty() {
        out += &format!("  The same: {scope}.\n");
        return out;
    }
    if real.is_empty() {
        out += &format!(
            "  The same apart from {}, of {scope}: for each, one rerun of the bundled engine, meeting titles in \
             another order, gives the built-in engine's result for it and everything that depends on it.\n",
            count(order_sensitive, "order-sensitive difference"),
        );
        return out;
    }
    let headline = match r.verdict {
        Verdict::Inconclusive => format!(
            "INCONCLUSIVE: {} of {scope}, {order_sensitive} of them order-sensitive. Reruns reproduced some of \
             the rest but no rerun all of it, after {}; try more with --orders:",
            count(r.differences.len(), "difference"),
            count(r.set_orders as usize, "other order"),
        ),
        _ => format!(
            "DIFFERENT: {} of {scope}, {order_sensitive} of them order-sensitive. Not explained by order, after \
             {}:",
            count(r.differences.len(), "difference"),
            count(r.set_orders as usize, "other order"),
        ),
    };
    out += &format!("  {headline}\n");
    for d in real {
        out += &format!("  {}\n", d.unit.replace('\t', " › "));
        out += &lines_diff(d.bundled.as_deref(), d.builtin.as_deref());
    }
    out
}

/// The lines only one engine has, each marked with the engine.
fn lines_diff(bundled: Option<&str>, builtin: Option<&str>) -> String {
    let (a, b): (Vec<&str>, Vec<&str>) = (
        bundled.map_or_else(Vec::new, |s| s.lines().collect()),
        builtin.map_or_else(Vec::new, |s| s.lines().collect()),
    );
    let mut out = String::new();
    if a.is_empty() {
        out += "    bundled:  (nothing)\n";
    }
    for line in a.iter().filter(|l| !b.contains(l)) {
        out += &format!("    bundled:  {line}\n");
    }
    if b.is_empty() {
        out += "    built-in: (nothing)\n";
    }
    for line in b.iter().filter(|l| !a.contains(l)) {
        out += &format!("    built-in: {line}\n");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_real_difference_shows_each_engines_lines_and_an_order_sensitive_one_only_counts() {
        let report = Report {
            dat: PathBuf::from("Test.dat"),
            verdict: Verdict::Different,
            conforms: false,
            groups: 2,
            units: 9,
            set_orders: 32,
            bundled_error: None,
            builtin_error: None,
            differences: vec![
                Difference {
                    unit: "group pong\tPong (USA)".into(),
                    group: Some("pong".into()),
                    order_sensitive: false,
                    witness: None,
                    bundled: Some("winner".into()),
                    builtin: None,
                },
                Difference {
                    unit: "trace group combat".into(),
                    group: Some("combat".into()),
                    order_sensitive: true,
                    witness: Some(3),
                    bundled: Some("a".into()),
                    builtin: Some("b".into()),
                },
            ],
        };

        assert_eq!(
            text(&report),
            "Test.dat\n  DIFFERENT: 2 differences of 9 checks across 2 groups, 1 of them order-sensitive. Not \
             explained by order, after 32 other orders:\n  group pong › Pong (USA)\n    bundled:  winner\n    \
             built-in: (nothing)\n"
        );
    }

    #[test]
    fn engines_that_fail_differently_are_inconclusive() {
        let report = Report {
            dat: PathBuf::from("Odd.dat"),
            verdict: Verdict::Inconclusive,
            conforms: false,
            groups: 0,
            units: 0,
            set_orders: 0,
            bundled_error: Some("EOFError".into()),
            builtin_error: Some("Odd.dat isn't UTF-8".into()),
            differences: Vec::new(),
        };
        assert_eq!(
            text(&report),
            "Odd.dat\n  INCONCLUSIVE: both engines failed, for different or unrecognised reasons.\n    bundled: \
             EOFError\n    built-in: Odd.dat isn't UTF-8\n"
        );
    }
}
