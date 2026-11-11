//! The names a Selection Profile may use, from the clone lists' `internal-config.json`, and the checks on them.

use std::collections::BTreeSet;

use title_select_api::selection::SelectionProfile;

use crate::Error;

/// The region, language and video names the engine knows, read from `internal-config.json`.
pub(crate) struct Vocabulary {
    pub regions: Vec<String>,
    pub languages: BTreeSet<String>,
    pub videos: Vec<String>,
}

/// A JSON object's keys in document order. Retool's default region order is the key order of an object, which
/// `serde_json::Map` would sort.
struct Keys(Vec<String>);

impl<'de> serde::Deserialize<'de> for Keys {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visitor;
        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = Keys;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("an object")
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(self, mut map: A) -> Result<Keys, A::Error> {
                let mut keys = Vec::new();
                while let Some((key, serde::de::IgnoredAny)) = map.next_entry::<String, _>()? {
                    keys.push(key);
                }
                Ok(Keys(keys))
            }
        }
        deserializer.deserialize_map(Visitor)
    }
}

impl Vocabulary {
    pub fn parse(internal_config: &[u8]) -> Result<Self, Error> {
        #[derive(serde::Deserialize)]
        struct Internal {
            #[serde(rename = "defaultRegionOrder")]
            regions: Keys,
            languages: Keys,
            #[serde(rename = "defaultVideoOrder")]
            videos: Vec<String>,
        }
        let internal: Internal = serde_json::from_slice(internal_config)
            .map_err(|e| Error::Install(format!("internal-config.json: {e}")))?;
        Ok(Vocabulary {
            regions: internal.regions.0,
            languages: internal.languages.0.into_iter().collect(),
            videos: internal.videos,
        })
    }

    /// The region order a Selection uses for `order`: Retool's default order when it is empty.
    pub fn region_order<'a>(&'a self, order: &'a [String]) -> &'a [String] {
        if order.is_empty() {
            &self.regions
        } else {
            order
        }
    }

    /// The video order a Selection uses for `order`: Retool's default order when it is empty.
    pub fn video_order<'a>(&'a self, order: &'a [String]) -> &'a [String] {
        if order.is_empty() {
            &self.videos
        } else {
            order
        }
    }

    /// Checks every name in the Profile, so that a typo is an error rather than a silently different Selection.
    pub fn check(&self, profile: &SelectionProfile) -> Result<(), Error> {
        let mut scopes = vec![(
            "profile".to_owned(),
            [
                Some(&profile.region_order),
                Some(&profile.language_order),
                Some(&profile.localization_order),
                Some(&profile.video_order),
            ],
        )];
        for (name, system) in &profile.systems {
            check_system_name(name)?;
            scopes.push((
                format!("system {name}"),
                [
                    system.region_order.as_ref(),
                    system.language_order.as_ref(),
                    system.localization_order.as_ref(),
                    system.video_order.as_ref(),
                ],
            ));
        }
        for (scope, [regions, languages, localizations, videos]) in scopes {
            type Known<'a> = &'a dyn Fn(&str) -> bool;
            let checks: [(&str, Option<&Vec<String>>, Known); 4] = [
                ("regions", regions, &|n| self.regions.iter().any(|r| r == n)),
                ("languages", languages, &|n| self.languages.contains(n)),
                ("localization languages", localizations, &|n| {
                    self.languages.contains(n)
                }),
                ("video standards", videos, &|n| {
                    self.videos.iter().any(|v| v == n)
                }),
            ];
            for (what, names, known) in checks {
                let unknown: Vec<&str> = names
                    .into_iter()
                    .flatten()
                    .map(String::as_str)
                    .filter(|n| !known(n))
                    .collect();
                if !unknown.is_empty() {
                    return Err(Error::Profile(format!(
                        "{scope}: unknown {what}: {}",
                        unknown.join(", ")
                    )));
                }
            }
        }
        Ok(())
    }
}

/// A system name as a DAT's header could give it: not empty, no path and no control characters.
fn check_system_name(name: &str) -> Result<(), Error> {
    if name.is_empty()
        || name.starts_with('.')
        || name.contains(['/', '\\'])
        || name.chars().any(char::is_control)
    {
        return Err(Error::Profile(format!("invalid system name {name:?}")));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use title_select_api::selection::SystemSettings;

    fn vocabulary() -> Vocabulary {
        Vocabulary {
            regions: vec!["USA".into(), "Europe".into(), "Japan".into()],
            languages: ["English".into(), "Japanese".into()].into(),
            videos: vec!["NTSC".into(), "PAL".into()],
        }
    }

    #[test]
    fn the_default_region_order_keeps_retools_order() {
        let parsed = Vocabulary::parse(
            br#"{"defaultRegionOrder": {"USA": {}, "Europe": {}, "Asia": {}}, "languages": {"Zulu": "zu", "Afrikaans": "af"}, "defaultVideoOrder": ["PAL"]}"#,
        )
        .unwrap();
        assert_eq!(parsed.regions, ["USA", "Europe", "Asia"]);
    }

    #[test]
    fn an_empty_order_is_the_default_order() {
        assert_eq!(vocabulary().region_order(&[]), ["USA", "Europe", "Japan"]);
        assert_eq!(vocabulary().video_order(&["PAL".into()]), ["PAL"]);
    }

    #[test]
    fn unknown_names_are_refused() {
        let mut profile = SelectionProfile {
            region_order: vec!["USA".into(), "Atlantis".into()],
            ..Default::default()
        };
        let err = vocabulary().check(&profile).unwrap_err().to_string();
        assert!(err.contains("Atlantis"), "{err}");
        profile.region_order.clear();
        profile
            .systems
            .insert("../escape".into(), SystemSettings::default());
        assert!(vocabulary().check(&profile).is_err());
    }
}
