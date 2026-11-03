//! A list's view state in the URL's query string, so a link or a reload keeps it: its filter (`q`), its sort
//! (`sort`, `-` first when descending), its selected row (`row`) and its page's tab (`tab`). Other query parameters,
//! such as a plan's id, are kept as they are.

use leptos::prelude::*;
use leptos_router::NavigateOptions;
use leptos_router::hooks::{use_location, use_navigate, use_query_map};

/// A sort by one column.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sort {
    pub column: String,
    pub descending: bool,
}

/// A list's view state.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ListQuery {
    pub filter: String,
    pub sort: Option<Sort>,
    pub selected: Option<String>,
    pub tab: Option<String>,
}

const FILTER: &str = "q";
const SORT: &str = "sort";
const ROW: &str = "row";
const TAB: &str = "tab";
const KEYS: [&str; 4] = [FILTER, SORT, ROW, TAB];

impl ListQuery {
    /// The state a query string's pairs hold; an empty value counts as none.
    pub fn from_pairs<'a>(pairs: impl IntoIterator<Item = (&'a str, &'a str)>) -> Self {
        let mut q = ListQuery::default();
        for (key, value) in pairs {
            let some = (!value.is_empty()).then(|| value.to_string());
            match key {
                FILTER => q.filter = value.to_string(),
                SORT => {
                    q.sort = some.map(|v| match v.strip_prefix('-') {
                        Some(column) => Sort {
                            column: column.to_string(),
                            descending: true,
                        },
                        None => Sort {
                            column: v,
                            descending: false,
                        },
                    })
                }
                ROW => q.selected = some,
                TAB => q.tab = some,
                _ => {}
            }
        }
        q
    }

    /// The pairs that hold this state, leaving out what is at its default.
    pub fn to_pairs(&self) -> Vec<(&'static str, String)> {
        let sort = self
            .sort
            .as_ref()
            .map(|s| format!("{}{}", if s.descending { "-" } else { "" }, s.column));
        [
            (FILTER, Some(self.filter.clone()).filter(|f| !f.is_empty())),
            (SORT, sort),
            (ROW, self.selected.clone()),
            (TAB, self.tab.clone()),
        ]
        .into_iter()
        .filter_map(|(k, v)| v.map(|v| (k, v)))
        .collect()
    }

    /// `others` without this state's keys, followed by this state.
    pub fn merged<'a>(
        &self,
        others: impl IntoIterator<Item = (&'a str, &'a str)>,
    ) -> Vec<(String, String)> {
        others
            .into_iter()
            .filter(|(k, _)| !KEYS.contains(k))
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .chain(self.to_pairs().into_iter().map(|(k, v)| (k.to_string(), v)))
            .collect()
    }

    /// Sorted by `column`: ascending first, then descending, then ascending again.
    pub fn sorted_by(mut self, column: &str) -> Self {
        let descending = self
            .sort
            .as_ref()
            .is_some_and(|s| s.column == column && !s.descending);
        self.sort = Some(Sort {
            column: column.to_string(),
            descending,
        });
        self
    }
}

/// Percent-encodes a query string component, keeping only RFC 3986's unreserved characters.
pub fn encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// `?k=v&…`, or nothing for no pairs.
pub fn query_string(pairs: &[(String, String)]) -> String {
    if pairs.is_empty() {
        return String::new();
    }
    let joined: Vec<String> = pairs
        .iter()
        .map(|(k, v)| format!("{}={}", encode(k), encode(v)))
        .collect();
    format!("?{}", joined.join("&"))
}

/// The current list state from the URL, and a way to change it. Changing it replaces the history entry rather than
/// adding one, and doesn't scroll.
pub fn use_list_query() -> (Memo<ListQuery>, Callback<ListQuery>) {
    let query = use_query_map();
    let state = Memo::new(move |_| {
        query.with(|q| ListQuery::from_pairs(q.latest_values().map(|(k, v)| (k.as_ref(), v))))
    });
    let location = use_location();
    let target = RwSignal::new(None::<String>);
    let navigate = use_navigate();
    Effect::new(move || {
        if let Some(path) = target.get() {
            navigate(
                &path,
                NavigateOptions {
                    replace: true,
                    scroll: false,
                    ..Default::default()
                },
            );
        }
    });
    let set = Callback::new(move |next: ListQuery| {
        let pairs =
            query.with_untracked(|q| next.merged(q.latest_values().map(|(k, v)| (k.as_ref(), v))));
        let path = location.pathname.get_untracked();
        target.set(Some(format!("{path}{}", query_string(&pairs))));
    });
    (state, set)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_list_query_survives_a_round_trip() {
        let q = ListQuery {
            filter: "Zelda & Link".to_string(),
            sort: Some(Sort {
                column: "name".to_string(),
                descending: true,
            }),
            selected: Some("12".to_string()),
            tab: Some("games".to_string()),
        };
        let pairs = q.to_pairs();
        assert_eq!(
            ListQuery::from_pairs(pairs.iter().map(|(k, v)| (*k, v.as_str()))),
            q
        );
        assert_eq!(pairs[1], ("sort", "-name".to_string()));
    }

    #[test]
    fn defaults_stay_out_of_the_url() {
        assert!(ListQuery::default().to_pairs().is_empty());
        assert_eq!(
            ListQuery::from_pairs([("q", ""), ("sort", ""), ("row", ""), ("tab", "")]),
            ListQuery::default()
        );
        assert_eq!(query_string(&[]), "");
    }

    #[test]
    fn other_parameters_are_kept_and_its_own_replaced() {
        let q = ListQuery {
            filter: "mario".to_string(),
            ..Default::default()
        };
        assert_eq!(
            q.merged([("plan", "7"), ("q", "old"), ("row", "3")]),
            vec![
                ("plan".to_string(), "7".to_string()),
                ("q".to_string(), "mario".to_string()),
            ]
        );
    }

    #[test]
    fn sorting_a_column_again_reverses_it() {
        let q = ListQuery::default().sorted_by("name");
        assert_eq!(q.sort.as_ref().map(|s| s.descending), Some(false));
        let q = q.sorted_by("name");
        assert_eq!(q.sort.as_ref().map(|s| s.descending), Some(true));
        let q = q.sorted_by("name");
        assert_eq!(q.sort.as_ref().map(|s| s.descending), Some(false));
        let q = q.sorted_by("size");
        assert_eq!(
            q.sort,
            Some(Sort {
                column: "size".to_string(),
                descending: false
            })
        );
    }

    #[test]
    fn query_strings_are_percent_encoded() {
        let pairs = vec![("q".to_string(), "Pokémon: Red & Blue/1".to_string())];
        assert_eq!(
            query_string(&pairs),
            "?q=Pok%C3%A9mon%3A%20Red%20%26%20Blue%2F1"
        );
    }
}
