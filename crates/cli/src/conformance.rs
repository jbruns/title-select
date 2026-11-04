//! `title-select conformance`: the built-in selection engine against the bundled one, DAT by DAT.

use std::path::{Path, PathBuf};

use serde::Serialize;
use title_select_api::selection::SelectionProfile;
use title_select_selection::{Conformance, Engine, RunOptions};

/// One DAT's comparison, as `--json` prints it.
#[derive(Serialize)]
struct Report {
    dat: PathBuf,
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
    bundled: Option<String>,
    builtin: Option<String>,
}

impl Report {
    fn new(dat: &Path, c: &Conformance) -> Report {
        Report {
            dat: dat.to_path_buf(),
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
                    order_sensitive: c.order_sensitive.contains(unit),
                    bundled: bundled.clone(),
                    builtin: builtin.clone(),
                })
                .collect(),
        }
    }
}

/// Compares the engines on each DAT, printing each DAT's result as it comes, then a summary. Fails when any DAT
/// has a difference that isn't order-sensitive, or only one engine failed on it.
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
        let order_only = reports
            .iter()
            .filter(|r| r.conforms && !r.differences.is_empty())
            .count();
        println!(
            "\n{} compared: {} the same, {} the same apart from order-sensitive differences, {} different.",
            count(reports.len(), "DAT"),
            reports.len() - differ - order_only,
            order_only,
            differ,
        );
    }
    match differ {
        0 => Ok(()),
        n => Err(format!(
            "the engines differ on {} of {}",
            n,
            count(reports.len(), "DAT")
        )),
    }
}

fn count(n: usize, what: &str) -> String {
    format!("{n} {what}{}", if n == 1 { "" } else { "s" })
}

/// One DAT's result as text: a headline, then each real difference with both engines' versions.
fn text(r: &Report) -> String {
    let mut out = format!("{}\n", r.dat.display());
    match (&r.bundled_error, &r.builtin_error) {
        (Some(a), Some(b)) => {
            out += &format!(
                "  Both engines failed, so they agree.\n    bundled: {a}\n    built-in: {b}\n"
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
            "  The same apart from {}, of {scope}: the bundled engine gives the built-in engine's result when it \
             meets titles in another order.\n",
            count(order_sensitive, "order-sensitive difference"),
        );
        return out;
    }
    out += &format!(
        "  DIFFERENT: {} of {scope}, {order_sensitive} of them order-sensitive. Not explained by order, \
         after {}:\n",
        count(r.differences.len(), "difference"),
        count(r.set_orders as usize, "other order"),
    );
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
                    bundled: Some("winner".into()),
                    builtin: None,
                },
                Difference {
                    unit: "trace group combat".into(),
                    group: Some("combat".into()),
                    order_sensitive: true,
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
}
