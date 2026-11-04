//! Python's `str` operations where Rust's differ: whitespace, slicing by code point, `int()`.

/// A Python exception Retool doesn't catch, which ends its run with an error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Crash(pub String);

pub(crate) type PyResult<T> = Result<T, Crash>;

/// `str.isspace()` for one character.
pub(crate) fn is_space(c: char) -> bool {
    c.is_whitespace() || ('\x1c'..='\x1f').contains(&c)
}

/// `str.strip()`.
pub(crate) fn strip(s: &str) -> &str {
    s.trim_matches(is_space)
}

/// `str.rstrip()`.
pub(crate) fn rstrip(s: &str) -> &str {
    s.trim_end_matches(is_space)
}

/// `s[start:end]` by code point, with Python's negative indices and clamping.
pub(crate) fn slice(s: &str, start: Option<isize>, end: Option<isize>) -> String {
    let chars: Vec<char> = s.chars().collect();
    let len = chars.len() as isize;
    let clamp = |i: isize| -> usize {
        let i = if i < 0 { i + len } else { i };
        i.clamp(0, len) as usize
    };
    let start = start.map_or(0, clamp);
    let end = end.map_or(len as usize, clamp);
    if start >= end {
        return String::new();
    }
    chars[start..end].iter().collect()
}

/// `int(s)` for a decimal string.
pub(crate) fn int(s: &str) -> PyResult<i64> {
    let t = strip(s);
    let (negative, digits) = match t.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, t.strip_prefix('+').unwrap_or(t)),
    };
    let mut value: i64 = 0;
    let mut previous_underscore = true;
    let mut any = false;
    for c in digits.chars() {
        if c == '_' && !previous_underscore {
            previous_underscore = true;
            continue;
        }
        let Some(d) = c.to_digit(10) else {
            return Err(Crash(format!(
                "ValueError: invalid literal for int() with base 10: {s:?}"
            )));
        };
        value = value.saturating_mul(10).saturating_add(i64::from(d));
        previous_underscore = false;
        any = true;
    }
    if !any || previous_underscore {
        return Err(Crash(format!(
            "ValueError: invalid literal for int() with base 10: {s:?}"
        )));
    }
    Ok(if negative { -value } else { value })
}

/// `str.find(sub)` as a code point index, or -1.
pub(crate) fn find(s: &str, sub: &str) -> isize {
    s.find(sub)
        .map_or(-1, |byte| s[..byte].chars().count() as isize)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slices_and_ints_follow_python() {
        assert_eq!(slice("(19991231)", Some(1), Some(-5)), "1999");
        assert_eq!(slice("abc", Some(5), None), "");
        assert_eq!(slice("abc", None, Some(-1)), "ab");
        assert_eq!(int(" 07 "), Ok(7));
        assert!(int("").is_err());
        assert_eq!(int("1_000"), Ok(1000));
        assert!(int("1__0").is_err());
        assert_eq!(strip("\x1c a \u{3000}"), "a");
    }
}
