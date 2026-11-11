//! The Operator's own regular expressions (`/pattern` lines in overrides and post filters), which Retool searches
//! with `re.search(pattern, name, flags=re.I)`.
//!
//! Retool's own patterns are fixed, and the differential tests show `fancy_regex` reads them as Python does. The
//! Operator's can be anything Python accepts, and `fancy_regex` reads some of that differently: set operations and
//! POSIX classes in brackets, `\<`, `\N{…}`, conditionals, `\w` and `\s` over Unicode, `$` before a final newline.
//! So these are parsed as Python's `sre_parse` reads them and rewritten into a pattern with the same meaning, and
//! anything this can't rewrite faithfully is refused rather than matched differently.

/// A character class the pattern uses, in Python's meaning.
#[derive(Clone, Copy)]
enum Class {
    Digit,
    Word,
    Space,
}

/// What a pattern compiles to: the rewritten pattern, and whether it matches case-insensitively by itself (ASCII
/// mode spells its case folding out, so it compiles case-sensitively).
pub(crate) struct Rewritten {
    pub pattern: String,
    pub case_insensitive: bool,
}

struct Parser<'a> {
    chars: Vec<char>,
    at: usize,
    out: String,
    /// `re.ASCII`, from a leading `(?a)`.
    ascii: bool,
    /// `re.MULTILINE`, from a leading `(?m)`.
    multiline: bool,
    /// Groups open around the one being read.
    depth: usize,
    source: &'a str,
}

/// The deepest nesting of groups a pattern may have. The parser here recurses once per group, so a bound keeps a
/// pattern from overflowing the stack. `fancy_regex` refuses nesting from 64 groups, counting the lookahead a `$`
/// becomes, so this stays below it and the refusal names the limit; no real title pattern comes near it.
const MAX_DEPTH: usize = 60;

type Result<T> = std::result::Result<T, String>;

/// Rewrites `pattern`, which Python compiles with `re.I`, or says why it can't.
pub(crate) fn rewrite(pattern: &str) -> Result<Rewritten> {
    let mut p = Parser {
        chars: pattern.chars().collect(),
        at: 0,
        out: String::new(),
        ascii: false,
        multiline: false,
        depth: 0,
        source: pattern,
    };
    p.leading_flags()?;
    p.sequence()?;
    if p.peek().is_some() {
        // Only a `)` with no group to close stops the outermost sequence early.
        return unsupported("an unbalanced parenthesis");
    }
    if p.multiline {
        p.out.insert_str(0, "(?m)");
    }
    Ok(Rewritten {
        pattern: p.out,
        case_insensitive: !p.ascii,
    })
}

fn unsupported<T>(what: &str) -> Result<T> {
    Err(what.to_owned())
}

fn hex(c: char) -> String {
    format!("\\x{{{:x}}}", c as u32)
}

impl Parser<'_> {
    fn peek(&self) -> Option<char> {
        self.chars.get(self.at).copied()
    }

    fn peek_at(&self, offset: usize) -> Option<char> {
        self.chars.get(self.at + offset).copied()
    }

    fn next(&mut self) -> Option<char> {
        let c = self.peek();
        self.at += 1;
        c
    }

    fn starts_with(&self, text: &str) -> bool {
        text.chars()
            .enumerate()
            .all(|(i, c)| self.peek_at(i) == Some(c))
    }

    /// Python 3.11 takes global flags only at the start: `(?aimsux)`, possibly several groups of them.
    fn leading_flags(&mut self) -> Result<()> {
        while self.starts_with("(?") {
            let mut end = self.at + 2;
            while end < self.chars.len() && self.chars[end].is_ascii_alphabetic() {
                end += 1;
            }
            if end == self.at + 2 || self.chars.get(end) != Some(&')') {
                return Ok(());
            }
            for &flag in &self.chars[self.at + 2..end] {
                match flag {
                    'a' => self.ascii = true,
                    'm' => self.multiline = true,
                    'i' | 'u' => {}
                    's' => self.out.push_str("(?s)"),
                    'x' => return unsupported("verbose mode (?x)"),
                    'L' => return unsupported("locale mode (?L)"),
                    _ => return unsupported("an unknown flag"),
                }
            }
            self.at = end + 1;
        }
        Ok(())
    }

    /// Everything up to the end, or to the `)` that closes the group being read.
    fn sequence(&mut self) -> Result<()> {
        while let Some(c) = self.peek() {
            match c {
                ')' => return Ok(()),
                '(' => self.group()?,
                '[' => {
                    self.next();
                    self.class()?;
                }
                '\\' => {
                    self.next();
                    self.escape()?;
                }
                '{' => self.brace(),
                '$' => {
                    self.next();
                    // Python's `$` also matches before a final newline.
                    self.out
                        .push_str(if self.multiline { "$" } else { "(?=\\n?\\z)" });
                }
                '*' | '+' | '?' | '|' | '^' | '.' => {
                    self.next();
                    self.out.push(c);
                }
                _ => {
                    self.next();
                    self.literal(c)?;
                }
            }
        }
        Ok(())
    }

    fn group(&mut self) -> Result<()> {
        if self.depth == MAX_DEPTH {
            return unsupported(&format!("more than {MAX_DEPTH} nested groups"));
        }
        self.next();
        if self.peek() != Some('?') {
            self.out.push('(');
        } else if self.starts_with("?:")
            || self.starts_with("?=")
            || self.starts_with("?!")
            || self.starts_with("?>")
        {
            self.out.push('(');
            for _ in 0..2 {
                let c = self.next().expect("group kind");
                self.out.push(c);
            }
        } else if self.starts_with("?<=") || self.starts_with("?<!") {
            self.out.push('(');
            for _ in 0..3 {
                let c = self.next().expect("lookbehind");
                self.out.push(c);
            }
        } else if self.starts_with("?P<") {
            self.at += 3;
            let name = self.name('>')?;
            self.out.push_str(&format!("(?P<{name}>"));
        } else if self.starts_with("?P=") {
            if self.ascii {
                return unsupported("a backreference in ASCII mode");
            }
            self.at += 3;
            let name = self.name(')')?;
            self.out.push_str(&format!("(?P={name})"));
            return Ok(());
        } else if self.starts_with("?#") {
            while let Some(c) = self.next() {
                if c == ')' {
                    return Ok(());
                }
            }
            return unsupported("an unterminated comment");
        } else if self.starts_with("?(") {
            return unsupported("a conditional group (?(…)…)");
        } else {
            // Scoped flags, `(?i-s:…)`.
            self.next();
            let mut flags = String::new();
            while let Some(c) = self.peek() {
                if c == ':' {
                    break;
                }
                match c {
                    'i' | 's' | '-' if !self.ascii => flags.push(c),
                    'i' | 's' | '-' => return unsupported("scoped flags in ASCII mode"),
                    'm' => return unsupported("a scoped multiline flag"),
                    'a' => return unsupported("a scoped ASCII flag"),
                    'x' => return unsupported("verbose mode (?x)"),
                    _ => return unsupported("this group syntax"),
                }
                self.next();
            }
            if self.next() != Some(':') {
                return unsupported("this group syntax");
            }
            self.out.push_str(&format!("(?{flags}:"));
        }
        self.depth += 1;
        let read = self.sequence();
        self.depth -= 1;
        read?;
        if self.next() != Some(')') {
            return unsupported("an unbalanced parenthesis");
        }
        self.out.push(')');
        Ok(())
    }

    fn name(&mut self, end: char) -> Result<String> {
        let mut name = String::new();
        loop {
            match self.next() {
                Some(c) if c == end => break,
                Some(c) if c.is_ascii_alphanumeric() || c == '_' => name.push(c),
                _ => return unsupported("this group name"),
            }
        }
        if name.is_empty() || name.starts_with(|c: char| c.is_ascii_digit()) {
            return unsupported("this group name");
        }
        Ok(name)
    }

    /// A `{`: a repeat when it reads as one, as Python's `{,n}` and `{m,}` do, else the character.
    fn brace(&mut self) {
        self.next();
        let digits = |p: &mut Self| {
            let mut d = String::new();
            while let Some(c) = p.peek().filter(char::is_ascii_digit) {
                d.push(c);
                p.next();
            }
            d
        };
        let start = self.at;
        let lo = digits(self);
        let (hi, comma) = if self.peek() == Some(',') {
            self.next();
            (digits(self), true)
        } else {
            (lo.clone(), false)
        };
        if self.peek() == Some('}') && (comma || !lo.is_empty()) {
            self.next();
            let lo = if lo.is_empty() { "0".into() } else { lo };
            self.out.push_str(&format!("{{{lo},{hi}}}"));
        } else {
            self.at = start;
            self.out.push_str(&hex('{'));
        }
    }

    /// A character to match as itself.
    fn literal(&mut self, c: char) -> Result<()> {
        if self.ascii {
            if c.is_ascii_alphabetic() {
                let (l, u) = (c.to_ascii_lowercase(), c.to_ascii_uppercase());
                self.out.push_str(&format!("[{l}{u}]"));
            } else if c.is_ascii_digit() {
                self.out.push(c);
            } else {
                self.out.push_str(&hex(c));
            }
        } else if c.is_alphanumeric() {
            self.out.push(c);
        } else {
            self.out.push_str(&hex(c));
        }
        Ok(())
    }

    fn class_text(&self, class: Class, negated: bool) -> String {
        let inner = match (class, self.ascii) {
            (Class::Digit, true) => "0-9",
            (Class::Digit, false) => "\\d",
            (Class::Word, true) => "0-9A-Za-z_",
            (Class::Word, false) => "\\p{L}\\p{N}_",
            (Class::Space, true) => "\\t\\n\\x{b}\\x{c}\\r ",
            (Class::Space, false) => "\\s\\x{1c}-\\x{1f}",
        };
        format!("[{}{inner}]", if negated { "^" } else { "" })
    }

    fn escape(&mut self) -> Result<()> {
        let Some(c) = self.next() else {
            return unsupported("a trailing backslash");
        };
        let word = self.class_text(Class::Word, false);
        let out = match c {
            'A' => "\\A".to_owned(),
            'Z' => "\\z".to_owned(),
            'b' => format!("(?:(?<={word})(?!{word})|(?<!{word})(?={word}))"),
            // As in `sre`, `\B` never matches in an empty string.
            'B' => format!("(?!\\A\\z)(?:(?<={word})(?={word})|(?<!{word})(?!{word}))"),
            'd' | 'D' | 'w' | 'W' | 's' | 'S' => {
                let class = match c.to_ascii_lowercase() {
                    'd' => Class::Digit,
                    'w' => Class::Word,
                    _ => Class::Space,
                };
                self.class_text(class, c.is_ascii_uppercase())
            }
            '1'..='9' => {
                if self.peek().is_some_and(|d| d.is_ascii_digit()) {
                    return unsupported("a two-digit backreference or an octal escape");
                }
                if self.ascii {
                    return unsupported("a backreference in ASCII mode");
                }
                format!("\\{c}")
            }
            _ => {
                let ch = self.char_escape(c)?;
                return self.literal(ch);
            }
        };
        self.out.push_str(&out);
        Ok(())
    }

    /// An escape that stands for one character, outside or inside brackets.
    fn char_escape(&mut self, c: char) -> Result<char> {
        let code = |p: &mut Self, n: usize| -> Result<char> {
            let digits: String = (0..n).filter_map(|_| p.next()).collect();
            if digits.len() != n || !digits.chars().all(|d| d.is_ascii_hexdigit()) {
                return unsupported("a malformed escape");
            }
            u32::from_str_radix(&digits, 16)
                .ok()
                .and_then(char::from_u32)
                .ok_or_else(|| "a malformed escape".to_owned())
        };
        Ok(match c {
            'a' => '\x07',
            'f' => '\x0c',
            'n' => '\n',
            'r' => '\r',
            't' => '\t',
            'v' => '\x0b',
            'x' => code(self, 2)?,
            'u' => code(self, 4)?,
            'U' => code(self, 8)?,
            '0' => {
                let mut value = 0;
                for _ in 0..2 {
                    match self.peek().and_then(|d| d.to_digit(8)) {
                        Some(d) => {
                            value = value * 8 + d;
                            self.next();
                        }
                        None => break,
                    }
                }
                char::from_u32(value).expect("an octal escape below 0o100")
            }
            'N' => return unsupported("a named character \\N{…}"),
            c if c.is_ascii_alphanumeric() => return unsupported("this escape"),
            c => c,
        })
    }

    /// A bracketed class, after its `[`.
    fn class(&mut self) -> Result<()> {
        let negated = self.peek() == Some('^');
        if negated {
            self.next();
        }
        // Each item: a range of characters, or a class's text.
        let mut ranges: Vec<(char, char)> = Vec::new();
        let mut classes: Vec<String> = Vec::new();
        let mut first = true;
        loop {
            let Some(c) = self.next() else {
                return unsupported("an unterminated bracket");
            };
            if c == ']' && !first {
                break;
            }
            first = false;
            let lo = if c == '\\' {
                let Some(e) = self.next() else {
                    return unsupported("a trailing backslash");
                };
                match e {
                    'd' | 'D' | 'w' | 'W' | 's' | 'S' => {
                        let class = match e.to_ascii_lowercase() {
                            'd' => Class::Digit,
                            'w' => Class::Word,
                            _ => Class::Space,
                        };
                        classes.push(self.class_text(class, e.is_ascii_uppercase()));
                        if self.peek() == Some('-') && self.peek_at(1).is_some_and(|n| n != ']') {
                            return unsupported("a range from a class");
                        }
                        continue;
                    }
                    'b' => '\x08',
                    '1'..='7' => {
                        let mut value = e.to_digit(8).expect("octal");
                        for _ in 0..2 {
                            match self.peek().and_then(|d| d.to_digit(8)) {
                                Some(d) => {
                                    value = value * 8 + d;
                                    self.next();
                                }
                                None => break,
                            }
                        }
                        char::from_u32(value)
                            .filter(|_| value <= 0o377)
                            .ok_or_else(|| "an octal escape".to_owned())?
                    }
                    _ => self.char_escape(e)?,
                }
            } else {
                c
            };
            if self.peek() == Some('-') && self.peek_at(1).is_some_and(|n| n != ']') {
                self.next();
                let hi = match self.next() {
                    Some('\\') => {
                        let e = self.next().unwrap_or('\\');
                        match e {
                            'b' => '\x08',
                            _ => self.char_escape(e)?,
                        }
                    }
                    Some(h) => h,
                    None => return unsupported("an unterminated bracket"),
                };
                if hi < lo {
                    return unsupported("a reversed range");
                }
                ranges.push((lo, hi));
            } else {
                ranges.push((lo, lo));
            }
        }
        let mut body = String::new();
        for (lo, hi) in ranges {
            if self.ascii {
                if !hi.is_ascii() {
                    return unsupported("a non-ASCII character in brackets in ASCII mode");
                }
                // Spell ASCII case folding out: the pattern compiles case-sensitively.
                for (a, z, shift) in [('a', 'z', -32i32), ('A', 'Z', 32)] {
                    let (l, h) = (lo.max(a), hi.min(z));
                    if l <= h {
                        let move_by =
                            |c: char| char::from_u32((c as i32 + shift) as u32).expect("ASCII");
                        body.push_str(&format!("{}-{}", hex(move_by(l)), hex(move_by(h))));
                    }
                }
            }
            body.push_str(&format!("{}-{}", hex(lo), hex(hi)));
        }
        for class in classes {
            body.push_str(&class);
        }
        self.out
            .push_str(&format!("[{}{body}]", if negated { "^" } else { "" }));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::super::pyre::Re;

    #[test]
    fn groups_may_nest_sixty_deep_and_no_deeper() {
        // The innermost group is a lookahead, and `$` becomes one more: both count towards `fancy_regex`'s limit.
        let nested = |depth: usize| {
            format!(
                "{}(?=Te)Test${}",
                "(?:".repeat(depth - 1),
                ")".repeat(depth - 1)
            )
        };
        let deepest = Re::user(&nested(super::MAX_DEPTH)).unwrap();
        assert!(deepest.search("A Test"));
        assert!(!deepest.search("A Tent"));
        let refused = Re::user(&nested(super::MAX_DEPTH + 1)).err().unwrap();
        assert_eq!(refused, "more than 60 nested groups");
    }

    #[test]
    fn a_closing_parenthesis_without_its_group_is_unbalanced() {
        for pattern in ["Test)NEVER", ")", "(a))", "a|b)"] {
            assert_eq!(
                Re::user(pattern).err().as_deref(),
                Some("an unbalanced parenthesis"),
                "{pattern}"
            );
        }
        assert!(Re::user("(a)(b)").is_ok());
    }

    /// Python's `re.search(pattern, subject, flags=re.I) is not None` for each of [`SUBJECTS`], or `None` where
    /// Python doesn't compile the pattern: what the bundled Retool's Python answered for each case, frozen in
    /// `tests/oracle/` (one JSON array per pattern: the pattern, then a `0`/`1` per subject or `null`).
    fn python(frozen: &str, patterns: &[&str]) -> Vec<Option<Vec<bool>>> {
        let rows: Vec<(String, Option<String>)> = frozen
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(rows.len(), patterns.len());
        rows.into_iter()
            .zip(patterns)
            .map(|((pattern, bits), want)| {
                assert_eq!(pattern, *want, "the frozen cases are these patterns");
                bits.map(|bits| {
                    assert_eq!(bits.len(), SUBJECTS.len());
                    bits.chars().map(|c| c == '1').collect()
                })
            })
            .collect()
    }

    const SUBJECTS: &[&str] = &[
        "Pong (USA)",
        "pong (usa)",
        "PONG",
        "Pokémon Red (USA)",
        "POKÉMON",
        "Super Mario Bros. 3 (USA) (Rev 1)",
        "a]",
        "a",
        ":",
        "&",
        "-",
        "~",
        "|",
        "<",
        "x{,3}",
        "xx",
        "A_B 2² ½",
        "A\u{1c}B",
        "Tab\there",
        "line\n",
        "\u{212a}elvin",
        "cla\u{17f}\u{17f}",
        "Street Fighter II' - Champion Edition",
        "Game [b]",
        "",
    ];

    #[test]
    fn user_patterns_match_as_python_reads_them_or_are_refused() {
        let patterns = [
            "^Pong",
            "(?a)^Pong",
            "(?a)^\\w+",
            "^\\w+$",
            "(?a)^\\w+\\s",
            "\\bUSA\\b",
            "(?a)\\bmon\\b",
            "\\Bmon",
            "[[:alpha:]]",
            "[a&&b]",
            "[a~~b]",
            "[a||b]",
            "\\<",
            "x{,3}",
            "^x{,3}$",
            "x{2,}",
            "x{}",
            "\\x41",
            "\\101",
            "\\0",
            "[\\d-z]",
            "[^a-z]",
            "(?a)[^a-z]",
            "(?a)[k-s]",
            "(?a)kelvin",
            "(?a)class",
            "\\s",
            "\\S\\s\\S",
            "line$",
            "(?m)line$",
            "\\Aa\\Z",
            "(?P<n>o)(?P=n)",
            "(o)\\1",
            "(?(1)a|b)",
            "(a)?(?(1)b|c)",
            "\\N{LATIN SMALL LETTER A}",
            "(?x) a # c",
            "(?i:pong)",
            "(?-i:pong)",
            "pok[é]mon",
            "(?a)pokémon",
            "(?a)pok[é]mon",
            "[\\w']+ - ",
            "\\(Rev \\d+\\)",
            "\\[b\\]",
            "[]a]",
            "[^]a]",
            "a**",
            "(?<=Super )Mario",
            "(?#comment)Pong",
            "\\d{1,2}²",
            "[\\b]",
            "\\e",
        ];
        let cases: Vec<(&str, Vec<&str>)> =
            patterns.iter().map(|p| (*p, SUBJECTS.to_vec())).collect();
        let expected = python(
            include_str!("../../tests/oracle/user-patterns.jsonl"),
            &patterns,
        );
        let mut refused = Vec::new();
        for ((pattern, subjects), expected) in cases.iter().zip(expected) {
            let ours = Re::user(pattern);
            match (ours, expected) {
                (Ok(re), Some(expected)) => {
                    for (subject, want) in subjects.iter().zip(expected) {
                        assert_eq!(
                            re.search(subject),
                            want,
                            "{pattern:?} on {subject:?}, rewritten as {:?}",
                            re.rewritten()
                        );
                    }
                }
                (Ok(re), None) => panic!(
                    "{pattern:?} is invalid in Python but accepted as {:?}",
                    re.rewritten()
                ),
                (Err(_), Some(_)) => refused.push(*pattern),
                (Err(_), None) => {}
            }
        }
        assert_eq!(
            refused,
            [
                "\\101",
                "(a)?(?(1)b|c)",
                "\\N{LATIN SMALL LETTER A}",
                "(?x) a # c",
                "(?a)pok[é]mon"
            ],
            "only these are refused"
        );
    }

    /// Random patterns from pieces of Python's syntax, against Python.
    #[test]
    fn generated_user_patterns_match_as_python_reads_them_or_are_refused() {
        const PIECES: &[&str] = &[
            "a", "o", "P", "k", "s", "é", "É", "2", " ", "-", "]", "[", "[^", "\\", "(", ")",
            "(?:", "(?=", "(?!", "(?<=a)", "(?P<g>", "(?P=g)", "|", "*", "+", "?", "*?", "{2}",
            "{,2}", "{1,}", "{", "}", "^", "$", ".", "\\w", "\\W", "\\d", "\\D", "\\s", "\\S",
            "\\b", "\\B", "\\A", "\\Z", "\\(", "\\)", "\\.", "\\x41", "\\u00e9", "\\0", "\\1",
            "&&", "--", "~~", "a-z", "(?a)", "(?i)", "(?m)", "(?s)", "²", "_",
        ];
        let mut rng = super::super::tests::Rng::new(94);
        let patterns: Vec<String> = (0..3000)
            .map(|_| {
                let n = 1 + rng.below(6);
                (0..n).map(|_| *rng.pick(PIECES)).collect()
            })
            .collect();
        let cases: Vec<(&str, Vec<&str>)> = patterns
            .iter()
            .map(|p| (p.as_str(), SUBJECTS.to_vec()))
            .collect();
        let (mut supported, mut refused) = (0, 0);
        let frozen = python(
            include_str!("../../tests/oracle/generated-patterns.jsonl"),
            &patterns.iter().map(String::as_str).collect::<Vec<_>>(),
        );
        for ((pattern, subjects), expected) in cases.iter().zip(frozen) {
            match (Re::user(pattern), expected) {
                (Ok(re), Some(expected)) => {
                    supported += 1;
                    for (subject, want) in subjects.iter().zip(expected) {
                        assert_eq!(
                            re.search(subject),
                            want,
                            "{pattern:?} on {subject:?}, rewritten as {:?}",
                            re.rewritten()
                        );
                    }
                }
                // A Profile with a pattern Python refused is refused before the engine runs.
                (_, None) => {}
                (Err(_), Some(_)) => refused += 1,
            }
        }
        assert!(
            supported > 1000 && refused < supported / 10,
            "{supported} supported, {refused} refused"
        );
    }
}
