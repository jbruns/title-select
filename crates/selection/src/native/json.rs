//! JSON as Python's `json` module reads and writes it: objects keep their key order, and `dumps` writes the
//! spacing and escapes Retool's trace has.

use indexmap::IndexMap;

#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
#[serde(untagged)]
pub(crate) enum J {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    List(Vec<J>),
    Dict(IndexMap<String, J>),
}

impl J {
    pub fn get(&self, key: &str) -> Option<&J> {
        match self {
            J::Dict(d) => d.get(key),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            J::Str(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_dict(&self) -> Option<&IndexMap<String, J>> {
        match self {
            J::Dict(d) => Some(d),
            _ => None,
        }
    }

    pub fn as_list(&self) -> Option<&Vec<J>> {
        match self {
            J::List(l) => Some(l),
            _ => None,
        }
    }

    /// Python's truthiness.
    pub fn truthy(&self) -> bool {
        match self {
            J::Null => false,
            J::Bool(b) => *b,
            J::Int(i) => *i != 0,
            J::Float(f) => *f != 0.0,
            J::Str(s) => !s.is_empty(),
            J::List(l) => !l.is_empty(),
            J::Dict(d) => !d.is_empty(),
        }
    }

    /// `str(value)` for the values Retool formats into messages.
    pub fn py_str(&self) -> String {
        match self {
            J::Str(s) => s.clone(),
            other => other.py_repr(),
        }
    }

    /// `repr(value)`.
    pub fn py_repr(&self) -> String {
        match self {
            J::Null => "None".into(),
            J::Bool(true) => "True".into(),
            J::Bool(false) => "False".into(),
            J::Int(i) => i.to_string(),
            J::Float(f) => format!("{f:?}"),
            J::Str(s) => py_repr(s),
            J::List(l) => format!(
                "[{}]",
                l.iter().map(J::py_repr).collect::<Vec<_>>().join(", ")
            ),
            J::Dict(d) => format!(
                "{{{}}}",
                d.iter()
                    .map(|(k, v)| format!("{}: {}", py_repr(k), v.py_repr()))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        }
    }
}

impl From<&str> for J {
    fn from(s: &str) -> J {
        J::Str(s.to_owned())
    }
}

impl From<String> for J {
    fn from(s: String) -> J {
        J::Str(s)
    }
}

impl From<Vec<String>> for J {
    fn from(list: Vec<String>) -> J {
        J::List(list.into_iter().map(J::Str).collect())
    }
}

impl From<Option<Vec<String>>> for J {
    fn from(list: Option<Vec<String>>) -> J {
        list.map_or(J::Null, J::from)
    }
}

/// `repr(s)` for a `str`.
pub(crate) fn py_repr(s: &str) -> String {
    let quote = if s.contains('\'') && !s.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut out = String::with_capacity(s.len() + 2);
    out.push(quote);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if (c as u32) < 0x20 || c as u32 == 0x7f => {
                out.push_str(&format!("\\x{:02x}", c as u32));
            }
            c if !is_printable(c) => {
                let n = c as u32;
                if n <= 0xff {
                    out.push_str(&format!("\\x{n:02x}"));
                } else if n <= 0xffff {
                    out.push_str(&format!("\\u{n:04x}"));
                } else {
                    out.push_str(&format!("\\U{n:08x}"));
                }
            }
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

/// `str.isprintable()` for the characters names hold: Python treats separators (other than the space) and
/// control, format, private-use and unassigned characters as unprintable. This covers the common ones.
fn is_printable(c: char) -> bool {
    let n = c as u32;
    !((0x80..0xa0).contains(&n)
        || n == 0xad
        || (0x2000..=0x200f).contains(&n) && n != 0x2010
        || (0x2028..=0x202f).contains(&n)
        || (0x205f..=0x206f).contains(&n)
        || n == 0x3000
        || n == 0xfeff
        || (0xe000..=0xf8ff).contains(&n)
        || (0xfff9..=0xfffb).contains(&n)
        || n == 0xa0
        || n == 0x1680)
}

/// `json.dumps(value, ensure_ascii=False, sort_keys=True)`.
pub(crate) fn dumps(value: &J) -> String {
    let mut out = String::new();
    write(value, &mut out);
    out
}

fn write(value: &J, out: &mut String) {
    match value {
        J::Null => out.push_str("null"),
        J::Bool(true) => out.push_str("true"),
        J::Bool(false) => out.push_str("false"),
        J::Int(i) => out.push_str(&i.to_string()),
        J::Float(f) => out.push_str(&format!("{f:?}")),
        J::Str(s) => write_str(s, out),
        J::List(list) => {
            out.push('[');
            for (i, item) in list.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                write(item, out);
            }
            out.push(']');
        }
        J::Dict(dict) => {
            let mut keys: Vec<&String> = dict.keys().collect();
            keys.sort();
            out.push('{');
            for (i, key) in keys.into_iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                write_str(key, out);
                out.push_str(": ");
                write(&dict[key], out);
            }
            out.push('}');
        }
    }
}

fn write_str(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

/// Builds a `J::Dict` from pairs.
pub(crate) fn dict<const N: usize>(pairs: [(&str, J); N]) -> J {
    J::Dict(pairs.into_iter().map(|(k, v)| (k.to_owned(), v)).collect())
}
