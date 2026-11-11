//! Checks the regular expressions in a Profile's overrides and post filters before a run, because Retool skipped an
//! invalid one with only a warning, which would make a silently different Selection. The engine reads each as
//! Python's `re` would (`native::userre`) and refuses what it can't read faithfully, so the check is the same.

use title_select_api::selection::{Overrides, SelectionProfile};

use crate::Error;
use crate::native::user_regex;

/// One regular expression in a Profile and where it is.
struct Regex<'a> {
    /// The line as written, such as `/^Pong` or `</^Pong>`.
    line: &'a str,
    /// The expression after the `/`.
    expression: &'a str,
    place: String,
}

fn regexes(profile: &SelectionProfile) -> Vec<Regex<'_>> {
    let mut scopes: Vec<(String, &Overrides, &[String])> =
        vec![("global".into(), &profile.overrides, &profile.post_filters)];
    for (name, system) in &profile.systems {
        scopes.push((name.clone(), &system.overrides, &system.post_filters));
    }
    let mut found = Vec::new();
    for (scope, overrides, post_filters) in scopes {
        push(
            &mut found,
            &overrides.exclude,
            format!("{scope} exclude overrides"),
            true,
        );
        push(
            &mut found,
            &overrides.include,
            format!("{scope} include overrides"),
            true,
        );
        push(
            &mut found,
            post_filters,
            format!("{scope} post filters"),
            false,
        );
    }
    found
}

fn push<'a>(found: &mut Vec<Regex<'a>>, lines: &'a [String], place: String, overrides: bool) {
    for line in lines {
        // Retool strips the `<…>` that extends an override to related releases before it matches.
        let unwrapped = match line.strip_prefix('<').and_then(|l| l.strip_suffix('>')) {
            Some(inner) if overrides => inner,
            _ => line.as_str(),
        };
        if let Some(expression) = unwrapped.strip_prefix('/') {
            found.push(Regex {
                line,
                expression,
                place: place.clone(),
            });
        }
    }
}

/// Refuses a Profile whose overrides or post filters hold a regular expression the engine can't read.
pub(crate) fn check(profile: &SelectionProfile) -> Result<(), Error> {
    let problems: Vec<String> = regexes(profile)
        .iter()
        .filter_map(|regex| {
            let problem = user_regex(regex.expression).err()?;
            Some(format!(
                "{line:?} in your {place} isn't a regular expression selection can read ({problem}). A line that \
                 starts with / is a regular expression: fix it, or remove the / to match the text as written.",
                line = regex.line,
                place = regex.place,
            ))
        })
        .collect();
    if problems.is_empty() {
        Ok(())
    } else {
        Err(Error::Patterns(problems))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use title_select_api::selection::SystemSettings;

    #[test]
    fn regexes_are_found_where_retool_reads_them() {
        let mut profile = SelectionProfile {
            overrides: Overrides {
                exclude: vec!["/^Pong".into(), "Pong".into(), "</(Demo)>".into()],
                include: vec!["|/not a regex".into()],
            },
            post_filters: vec!["/[".into(), "<not unwrapped>".into(), "</kept>".into()],
            ..Default::default()
        };
        profile.systems.insert(
            "Test - System".into(),
            SystemSettings {
                post_filters: vec!["/(".into()],
                ..Default::default()
            },
        );

        let found: Vec<(&str, &str, String)> = regexes(&profile)
            .into_iter()
            .map(|r| (r.line, r.expression, r.place))
            .collect();

        assert_eq!(
            found,
            [
                ("/^Pong", "^Pong", "global exclude overrides".to_owned()),
                ("</(Demo)>", "(Demo)", "global exclude overrides".to_owned()),
                ("/[", "[", "global post filters".to_owned()),
                ("/(", "(", "Test - System post filters".to_owned()),
            ]
        );
    }

    #[test]
    fn a_pattern_the_engine_cant_read_is_refused_with_where_it_is() {
        let profile = SelectionProfile {
            post_filters: vec!["/^Pong".into(), "/(".into(), "/(?x) a # c".into()],
            ..Default::default()
        };
        let Err(Error::Patterns(problems)) = check(&profile) else {
            panic!("refused");
        };
        assert_eq!(problems.len(), 2, "{problems:?}");
        assert!(
            problems[0]
                .starts_with("\"/(\" in your global post filters isn't a regular expression"),
            "{problems:?}"
        );
        assert!(problems[1].contains("verbose mode"), "{problems:?}");
        assert!(check(&SelectionProfile::default()).is_ok());
    }
}
