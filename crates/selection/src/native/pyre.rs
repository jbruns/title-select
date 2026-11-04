//! Python `re` semantics on top of `fancy_regex`, which supports the lookaround Retool's patterns use.
//!
//! Differences that remain: Python's `$` also matches before a final newline, and a few Python-only escapes
//! (`\Z`) are translated. Title names have no newlines, so neither changes a result in practice.

use fancy_regex::{Captures, Regex, RegexBuilder};

#[derive(Debug, Clone)]
pub(crate) struct Re {
    regex: Regex,
    /// The pattern as Retool wrote it, for messages and identity.
    pub source: String,
    /// Whether Retool compiled it with `re.IGNORECASE`.
    pub ignore_case: bool,
}

impl PartialEq for Re {
    fn eq(&self, other: &Self) -> bool {
        self.source == other.source && self.regex.as_str() == other.regex.as_str()
    }
}

/// Rewrites the Python-only syntax `fancy_regex` doesn't read.
fn translate(pattern: &str) -> String {
    let mut out = String::with_capacity(pattern.len());
    let mut chars = pattern.chars().peekable();
    let mut in_class = false;
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                let Some(next) = chars.next() else {
                    out.push('\\');
                    break;
                };
                match next {
                    'Z' if !in_class => out.push_str("\\z"),
                    _ => {
                        out.push('\\');
                        out.push(next);
                    }
                }
            }
            '[' if !in_class => {
                in_class = true;
                out.push(c);
                if chars.peek() == Some(&'^') {
                    out.push(chars.next().expect("peeked"));
                }
                if chars.peek() == Some(&']') {
                    chars.next();
                    out.push_str("\\]");
                }
            }
            '[' if in_class => out.push_str("\\["),
            ']' if in_class => {
                in_class = false;
                out.push(c);
            }
            _ => out.push(c),
        }
    }
    out
}

impl Re {
    pub fn new(pattern: &str) -> Result<Re, String> {
        Self::build(pattern, false)
    }

    /// `re.compile(pattern, flags=re.I)`.
    pub fn new_i(pattern: &str) -> Result<Re, String> {
        Self::build(pattern, true)
    }

    fn build(pattern: &str, case_insensitive: bool) -> Result<Re, String> {
        let regex = RegexBuilder::new(&translate(pattern))
            .case_insensitive(case_insensitive)
            .backtrack_limit(10_000_000)
            .build()
            .map_err(|e| e.to_string())?;
        Ok(Re {
            regex,
            source: pattern.to_owned(),
            ignore_case: case_insensitive,
        })
    }

    /// `re.compile(pattern, flags=re.I)` for one of the Operator's patterns, read as Python reads it; an error
    /// says what the built-in engine doesn't support (see `userre`).
    pub fn user(pattern: &str) -> Result<Re, String> {
        let rewritten = super::userre::rewrite(pattern)?;
        let regex = RegexBuilder::new(&rewritten.pattern)
            .case_insensitive(rewritten.case_insensitive)
            .backtrack_limit(10_000_000)
            .build()
            .map_err(|e| e.to_string())?;
        Ok(Re {
            regex,
            source: pattern.to_owned(),
            ignore_case: true,
        })
    }

    /// The pattern as compiled.
    pub fn rewritten(&self) -> &str {
        self.regex.as_str()
    }

    /// A pattern Retool builds itself, which is known to compile.
    pub fn of(pattern: &str) -> Re {
        Self::new(pattern).unwrap_or_else(|e| panic!("{pattern}: {e}"))
    }

    pub fn of_i(pattern: &str) -> Re {
        Self::new_i(pattern).unwrap_or_else(|e| panic!("{pattern}: {e}"))
    }

    /// `str(pattern)`: CPython's `re.compile(...)` repr, which cuts the pattern's repr at 200 characters.
    pub fn py_str(&self) -> String {
        let repr: String = super::json::py_repr(&self.source)
            .chars()
            .take(200)
            .collect();
        if self.ignore_case {
            format!("re.compile({repr}, re.IGNORECASE)")
        } else {
            format!("re.compile({repr})")
        }
    }

    fn captures<'t>(&self, text: &'t str) -> Option<Captures<'t, str>> {
        self.regex.captures(text).ok().flatten()
    }

    /// `re.search(pattern, text) is not None`.
    pub fn search(&self, text: &str) -> bool {
        self.regex.is_match(text).unwrap_or(false)
    }

    /// `re.match(pattern, text) is not None`: a match that starts at the start.
    pub fn matches_start(&self, text: &str) -> bool {
        self.regex
            .find_from_pos(text, 0)
            .ok()
            .flatten()
            .is_some_and(|m| m.start() == 0)
    }

    /// `pattern2string(pattern, text, group)`: the group of the first match, or `''`.
    pub fn group(&self, text: &str, group: usize) -> String {
        self.captures(text)
            .and_then(|c| c.get(group).map(|m| m.as_str().to_owned()))
            .unwrap_or_default()
    }

    /// `pattern2string(pattern, text)`.
    pub fn find(&self, text: &str) -> String {
        self.regex
            .find(text)
            .ok()
            .flatten()
            .map(|m| m.as_str().to_owned())
            .unwrap_or_default()
    }

    /// The byte span of the first match.
    pub fn span(&self, text: &str) -> Option<(usize, usize)> {
        self.regex
            .find(text)
            .ok()
            .flatten()
            .map(|m| (m.start(), m.end()))
    }

    /// The groups of the first match, with `None` for a group that didn't take part.
    pub fn captures_all(&self, text: &str) -> Option<Vec<Option<String>>> {
        self.captures(text).map(|c| {
            (0..c.len())
                .map(|i| c.get(i).map(|m| m.as_str().to_owned()))
                .collect()
        })
    }

    /// The matches, as `re.finditer` finds them.
    fn spans<'t>(&self, text: &'t str) -> Vec<Captures<'t, str>> {
        let mut out: Vec<Captures<'t, str>> = Vec::new();
        let mut pos = 0;
        while pos <= text.len() {
            let Some(caps) = self.regex.captures_from_pos(text, pos).ok().flatten() else {
                break;
            };
            let m = caps.get(0).expect("group 0");
            let (start, end) = (m.start(), m.end());
            out.push(caps);
            // Python allows an empty match right after a non-empty one, and steps past an empty one.
            pos = if start == end {
                next_char(text, end)
            } else {
                end
            };
        }
        out
    }

    /// `re.findall`: whole matches, or group 1 when the pattern has groups.
    pub fn findall(&self, text: &str) -> Vec<String> {
        self.spans(text)
            .into_iter()
            .map(|c| {
                let index = if c.len() > 1 { 1 } else { 0 };
                c.get(index)
                    .map(|m| m.as_str().to_owned())
                    .unwrap_or_default()
            })
            .collect()
    }

    /// `re.sub(pattern, replacement, text)`, with Python's `\1` and `\g<1>` template syntax.
    pub fn sub(&self, replacement: &str, text: &str) -> String {
        let mut out = String::with_capacity(text.len());
        let mut last = 0;
        for caps in self.spans(text) {
            let m = caps.get(0).expect("group 0");
            out.push_str(&text[last..m.start()]);
            expand(replacement, &caps, &mut out);
            last = m.end();
        }
        out.push_str(&text[last..]);
        out
    }
}

fn next_char(text: &str, at: usize) -> usize {
    text[at..]
        .chars()
        .next()
        .map_or(at + 1, |c| at + c.len_utf8())
}

fn expand(template: &str, caps: &Captures<'_, str>, out: &mut String) {
    let mut chars = template.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some(d) if d.is_ascii_digit() => {
                let mut number = d.to_digit(10).expect("digit") as usize;
                if let Some(e) = chars.peek().and_then(|e| e.to_digit(10)) {
                    chars.next();
                    number = number * 10 + e as usize;
                }
                if let Some(m) = caps.get(number) {
                    out.push_str(m.as_str());
                }
            }
            Some('g') if chars.peek() == Some(&'<') => {
                chars.next();
                let name: String = chars.by_ref().take_while(|c| *c != '>').collect();
                let m = match name.parse::<usize>() {
                    Ok(n) => caps.get(n),
                    Err(_) => caps.name(&name),
                };
                if let Some(m) = m {
                    out.push_str(m.as_str());
                }
            }
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('\\') => out.push('\\'),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
}

/// `re.escape`.
pub(crate) fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if c.is_ascii_alphanumeric() || c == '_' || !c.is_ascii() {
            out.push(c);
        } else {
            out.push('\\');
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn substitution_and_findall_follow_python() {
        assert_eq!(
            Re::of(r"\((\d{4}-\d{2})-xx\)").sub(r"(\1-01)", "A (1999-03-xx)"),
            "A (1999-03-01)"
        );
        assert_eq!(Re::of(r"\d+").findall("v1.20.3"), ["1", "20", "3"]);
        assert_eq!(
            Re::of(r"(v[\d+\.]+|\d{4})").findall("v1.2, 2001"),
            ["v1.2", "2001"]
        );
        assert_eq!(Re::of("x*").sub("-", "abxd"), "-a-b--d-");
        assert!(Re::of(r"(?<!^)\sV\d").search("Game V2"));
        assert!(!Re::of(r"b").matches_start("abc"));
        assert!(Re::of(r"a|b").matches_start("bc"));
    }
}
