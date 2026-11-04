//! Retool's JSON Lines trace (`TraceTools.json_*` in the patched Retool): the records `trace::derive` reads.
//!
//! Retool writes a step for each `trace_title` call and folds the console notes that follow it (`eprint`) into
//! the step's details, kept and removed lists. This reproduces that state machine, including which notes end a
//! step early.

use std::collections::{HashMap, HashSet};

use indexmap::IndexMap;

use super::json::{self, J, dict};
use super::py;
use super::pyre::Re;
use super::refs;

const CHAINED: [&str; 4] = [
    "Parent selection",
    "Superset parent selection",
    "Compilation parent selection",
    "Compilations",
];

struct Step {
    stage: String,
    group: String,
    scope: String,
    reference: String,
    message: String,
    before: Option<Vec<String>>,
    after: Option<Vec<String>>,
    kept: Vec<String>,
    removed: Vec<String>,
    variables: Vec<String>,
    details: Vec<String>,
}

impl Step {
    fn record(self) -> J {
        dict([
            ("type", "step".into()),
            ("stage", self.stage.into()),
            ("group", self.group.into()),
            ("scope", self.scope.into()),
            ("ref", self.reference.into()),
            ("message", self.message.into()),
            ("before", self.before.into()),
            ("after", self.after.into()),
            ("kept", self.kept.into()),
            ("removed", self.removed.into()),
            ("variables", self.variables.into()),
            ("details", self.details.into()),
        ])
    }
}

pub(crate) struct Trace {
    lines: Vec<String>,
    stage: String,
    group: String,
    scope: String,
    snapshots: HashMap<(String, String, String), Vec<String>>,
    group_names: HashMap<String, HashSet<String>>,
    /// Every title name Retool processed, with its group, in the order it met them.
    pub seen: IndexMap<String, String>,
    pending: Option<Step>,
    pub metadata: HashSet<String>,
    pub clone_list_unmatched: Vec<String>,
    /// Retool's warnings and errors, as lines.
    pub warnings: Vec<String>,
    ansi: Re,
    progress: Re,
}

/// `TraceTools.json_plain`.
pub(crate) fn plain(text: &str) -> String {
    let ansi = Re::of("\x1b(?:\\[[0-9;?]*[A-Za-z]|M)");
    py::strip(&ansi.sub("", text)).to_owned()
}

impl Trace {
    pub fn new() -> Trace {
        Trace {
            lines: Vec::new(),
            stage: String::new(),
            group: String::new(),
            scope: String::new(),
            snapshots: HashMap::new(),
            group_names: HashMap::new(),
            seen: IndexMap::new(),
            pending: None,
            metadata: HashSet::new(),
            clone_list_unmatched: Vec::new(),
            warnings: Vec::new(),
            ansi: Re::of("\x1b(?:\\[[0-9;?]*[A-Za-z]|M)"),
            progress: Re::of("• [A-Z][a-z]+ing\\b.*\\.\\.\\.( done\\.?)?$"),
        }
    }

    fn plain(&self, text: &str) -> String {
        py::strip(&self.ansi.sub("", text)).to_owned()
    }

    /// `TraceTools.json_context`.
    pub fn context(&mut self, stage: Option<&str>, group: &str, scope: &str) {
        if let Some(stage) = stage {
            self.stage = stage.to_owned();
        }
        self.group = group.to_owned();
        self.scope = scope.to_owned();
    }

    /// The current context, to restore later.
    pub fn current(&self) -> (String, String, String) {
        (self.stage.clone(), self.group.clone(), self.scope.clone())
    }

    pub fn write(&mut self, record: J) {
        self.flush();
        let J::Dict(mut record) = record else {
            unreachable!("records are objects")
        };
        record.insert("schema".into(), J::Int(1));
        self.lines.push(json::dumps(&J::Dict(record)));
    }

    pub fn flush(&mut self) {
        if let Some(step) = self.pending.take() {
            self.write(step.record());
        }
    }

    fn resolve(&self, text: &str, group: &str) -> Option<String> {
        let mut matches: Vec<&String> = self
            .group_names
            .get(group)
            .map(|names| {
                names
                    .iter()
                    .filter(|x| text.ends_with(x.as_str()))
                    .collect()
            })
            .unwrap_or_default();
        if matches.is_empty() {
            matches = self
                .seen
                .keys()
                .filter(|x| text.ends_with(x.as_str()))
                .collect();
        }
        let longest = matches.iter().map(|x| x.chars().count()).max()?;
        matches
            .into_iter()
            .find(|x| x.chars().count() == longest)
            .cloned()
    }

    /// `TraceTools.json_note`: a line of console output, folded into the pending step.
    fn note_line(&mut self, text: &str) {
        if self.pending.is_none() {
            return;
        }
        let line = self.plain(text);
        if line.is_empty() {
            return;
        }
        if self.progress.matches_start(&line) || line == "• Trace complete." {
            self.flush();
            return;
        }
        let group = self
            .pending
            .as_ref()
            .map(|p| p.group.clone())
            .unwrap_or_default();
        for (prefix, kept) in [("+ Keeping:", true), ("- Removing:", false)] {
            if let Some(rest) = line.strip_prefix(prefix)
                && let Some(name) = self.resolve(py::strip(rest), &group)
            {
                let step = self.pending.as_mut().expect("pending");
                if kept {
                    step.kept.push(name);
                } else {
                    step.removed.push(name);
                }
            }
        }
        self.pending.as_mut().expect("pending").details.push(line);
    }

    /// `eprint(text)` at the default or `disabled` level, which the trace folds in as notes.
    pub fn note(&mut self, text: &str) {
        for line in text.split('\n') {
            self.note_line(line);
        }
    }

    /// `eprint(text, level='warning')` or `level='error'`.
    pub fn warn(&mut self, text: &str) {
        for line in plain(text).split('\n') {
            let line = py::strip(line).trim_start_matches('•').trim();
            if !line.is_empty() {
                self.warnings.push(line.to_owned());
            }
        }
    }

    /// `TraceTools.json_step`, which `trace_title` calls. `titles` holds the names and groups of the titles
    /// the step reports, if any.
    pub fn step(
        &mut self,
        reference: &str,
        variables: &[String],
        titles: &[(String, String)],
        keep_remove: bool,
    ) {
        if reference.is_empty() && self.pending.is_some() {
            if keep_remove && variables.len() >= 2 {
                self.note_line(&format!("+ Keeping: {}", variables[0]));
                self.note_line(&format!("- Removing: {}", variables[1]));
            } else {
                let plain: Vec<String> = variables.iter().map(|v| self.plain(v)).collect();
                self.pending
                    .as_mut()
                    .expect("pending")
                    .variables
                    .extend(plain);
            }
            return;
        }
        let message = self.plain(&message(reference, variables));
        let mut names: Option<Vec<String>> = None;
        let mut group = self.group.clone();
        if !titles.is_empty() {
            let mut sorted: Vec<String> = titles.iter().map(|(n, _)| n.clone()).collect();
            sorted.sort();
            sorted.dedup();
            names = Some(sorted);
            if group.is_empty() {
                let groups: HashSet<&String> = titles.iter().map(|(_, g)| g).collect();
                if groups.len() == 1 {
                    group = groups.into_iter().next().expect("one").clone();
                }
            }
        }
        let key = (self.stage.clone(), group.clone(), self.scope.clone());
        let chained = CHAINED.contains(&self.stage.as_str());
        let before = if chained {
            self.snapshots.get(&key).cloned()
        } else {
            None
        };
        if let Some(names) = &names {
            if chained {
                self.snapshots.insert(key, names.clone());
            }
            self.group_names
                .entry(group.clone())
                .or_default()
                .extend(names.iter().cloned());
        }
        self.flush();
        self.pending = Some(Step {
            stage: self.stage.clone(),
            group,
            scope: self.scope.clone(),
            reference: reference.to_owned(),
            message,
            before,
            after: names,
            kept: Vec::new(),
            removed: Vec::new(),
            variables: Vec::new(),
            details: Vec::new(),
        });
        if keep_remove && variables.len() >= 2 {
            self.note_line(&format!("+ Keeping: {}", variables[0]));
            self.note_line(&format!("- Removing: {}", variables[1]));
            self.pending.as_mut().expect("pending").details.clear();
        } else {
            let plain: Vec<String> = variables.iter().map(|v| self.plain(v)).collect();
            self.pending.as_mut().expect("pending").variables = plain;
        }
    }

    /// `TraceTools.json_run`.
    pub fn run(&mut self, system: &str, clone_list: bool, metadata: HashSet<String>) {
        let any = !metadata.is_empty();
        self.metadata = metadata;
        self.write(dict([
            ("type", "run".into()),
            ("system", system.into()),
            ("clone_list", J::Bool(clone_list)),
            ("metadata", J::Bool(any)),
        ]));
    }

    /// The trace, a JSON record a line.
    pub fn finish(mut self) -> (String, Vec<String>) {
        self.flush();
        let mut out = self.lines.join("\n");
        if !out.is_empty() {
            out.push('\n');
        }
        (out, self.warnings)
    }
}

/// A reference's message with its variables in place.
fn message(reference: &str, variables: &[String]) -> String {
    let template = refs::template(reference);
    if template.contains("{list}") {
        let list = format!(
            "[{}]",
            variables
                .iter()
                .map(|v| json::py_repr(v))
                .collect::<Vec<_>>()
                .join(", ")
        );
        return template.replace("{list}", &list);
    }
    let mut out = String::new();
    let mut rest = template;
    while let Some(start) = rest.find('{') {
        out.push_str(&rest[..start]);
        let after = &rest[start..];
        match after
            .get(1..3)
            .and_then(|s| s.strip_suffix('}'))
            .and_then(|d| d.parse::<usize>().ok())
        {
            Some(i) => {
                out.push_str(variables.get(i).map_or("", String::as_str));
                rest = &after[3..];
            }
            None => {
                out.push('{');
                rest = &after[1..];
            }
        }
    }
    out.push_str(rest);
    out
}
