//! The settings a run reads, as Retool's `Config` holds them once it has read `internal-config.json`, the user
//! configuration and the system's settings file, and as its command line sets its options.
//!
//! The bundled engine reads YAML that `config.rs` writes from the Profile; this module reads the Profile directly
//! and reaches the same values, override markers resolved.

use std::collections::HashMap;

use indexmap::IndexMap;
use title_select_api::selection::{
    Category, Compilations, SelectionOptions, SelectionProfile, SystemSettings,
};

use super::pyre::Re;
use super::regexes::Regexes;
use crate::Error;

/// A tag Retool removes when it compares names: a literal string, or a pattern.
#[derive(Debug, Clone)]
pub(crate) enum Tag {
    String(String),
    Regex(Re),
}

impl Tag {
    /// The tag as Retool's lists hold it, for messages.
    pub fn text(&self) -> &str {
        match self {
            Tag::String(s) => s,
            Tag::Regex(r) => &r.source,
        }
    }
}

#[derive(serde::Deserialize)]
struct Internal {
    #[serde(rename = "ignoreTags")]
    ignore_tags: Vec<(String, String)>,
    #[serde(rename = "discRename")]
    disc_rename: IndexMap<String, String>,
    #[serde(rename = "versionIgnore")]
    version_ignore: Vec<String>,
    #[serde(rename = "budgetEditions")]
    budget_editions: Vec<(String, String)>,
    #[serde(rename = "demoteEditions")]
    demote_editions: Vec<(String, String)>,
    #[serde(rename = "modernEditions")]
    modern_editions: Vec<(String, String)>,
    #[serde(rename = "promoteEditions")]
    promote_editions: Vec<(String, String)>,
    languages: IndexMap<String, String>,
    #[serde(rename = "defaultRegionOrder")]
    default_region_order: IndexMap<String, Implied>,
    #[serde(rename = "defaultVideoOrder")]
    default_video_order: Vec<String>,
    #[serde(rename = "datFileTags")]
    dat_file_tags: Vec<String>,
}

#[derive(serde::Deserialize)]
struct Implied {
    #[serde(rename = "impliedLanguage")]
    implied_language: String,
}

fn tags(list: &[(String, String)]) -> Vec<Tag> {
    list.iter()
        .filter_map(|(pattern, kind)| match kind.as_str() {
            "regex" => Some(Tag::Regex(Re::of(pattern))),
            "string" => Some(Tag::String(pattern.clone())),
            _ => None,
        })
        .collect()
}

/// The `no_*` exclusions, by Retool's names.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Exclusions {
    pub add_ons: bool,
    pub aftermarket: bool,
    pub applications: bool,
    pub audio: bool,
    pub bad_dumps: bool,
    pub bios: bool,
    pub bonus_discs: bool,
    pub coverdiscs: bool,
    pub demos: bool,
    pub educational: bool,
    pub games: bool,
    pub manuals: bool,
    pub mia: bool,
    pub multimedia: bool,
    pub pirate: bool,
    pub preproduction: bool,
    pub promotional: bool,
    pub unlicensed: bool,
    pub video: bool,
}

impl Exclusions {
    fn of<'a>(categories: impl IntoIterator<Item = &'a Category>) -> Exclusions {
        let mut e = Exclusions::default();
        for category in categories {
            match category {
                Category::AddOns => e.add_ons = true,
                Category::Aftermarket => e.aftermarket = true,
                Category::Applications => e.applications = true,
                Category::Audio => e.audio = true,
                Category::BadDumps => e.bad_dumps = true,
                Category::Bios => e.bios = true,
                Category::BonusDiscs => e.bonus_discs = true,
                Category::Coverdiscs => e.coverdiscs = true,
                Category::Demos => e.demos = true,
                Category::Educational => e.educational = true,
                Category::Games => e.games = true,
                Category::Manuals => e.manuals = true,
                Category::Mia => e.mia = true,
                Category::Multimedia => e.multimedia = true,
                Category::Pirate => e.pirate = true,
                Category::Preproduction => e.preproduction = true,
                Category::Promotional => e.promotional = true,
                Category::Unlicensed => e.unlicensed = true,
                Category::Video => e.video = true,
            }
        }
        e
    }
}

/// The options Retool's `UserInput` holds that change a Selection.
#[derive(Debug, Clone, Default)]
pub(crate) struct Options {
    pub no_1g1r: bool,
    pub filter_languages: bool,
    pub oldest: bool,
    pub retroachievements: bool,
    pub region_bias: bool,
    pub demote_unl: bool,
    pub modern: bool,
    pub label_mia: bool,
    pub label_retro: bool,
    pub local_names: bool,
    /// `''`, `i`, `k` or `o`.
    pub compilations: &'static str,
    pub exclude: Exclusions,
}

fn options(options: &SelectionOptions, exclude: Exclusions) -> Options {
    Options {
        no_1g1r: options.disable_1g1r,
        filter_languages: options.filter_languages,
        oldest: options.prefer_oldest,
        retroachievements: options.prefer_retroachievements,
        region_bias: options.prefer_regions,
        demote_unl: options.prefer_licensed,
        modern: options.prefer_modern_rips,
        label_mia: true,
        label_retro: true,
        local_names: false,
        compilations: match options.compilations {
            Compilations::Default => "",
            Compilations::Individual => "i",
            Compilations::Keep => "k",
            Compilations::Optimize => "o",
        },
        exclude,
    }
}

/// A list a system's settings file may override. `Some` holds the system's own list when its `override: true`
/// marker is there.
pub(crate) type SystemOrder = Option<Vec<String>>;

pub(crate) struct Settings {
    pub regex: Regexes,
    /// Language names to their patterns, in internal-config order.
    pub languages: IndexMap<String, String>,
    /// With `United Kingdom` after `UK`.
    pub region_order_default: Vec<String>,
    pub languages_filter: HashMap<String, Vec<String>>,
    pub languages_implied: HashMap<String, Vec<String>>,
    /// `ignoreTags`, then the budget, promote, demote and modern editions.
    pub tags_ignore: Vec<Tag>,
    pub tags_budget_editions: Vec<Tag>,
    pub tags_promote_editions: Vec<Tag>,
    pub tags_demote_editions: Vec<Tag>,
    pub tags_modern_editions: Vec<Tag>,
    pub tags_disc_rename: Vec<(String, String)>,
    pub version_ignore: Vec<Re>,
    pub dat_file_tags: Vec<String>,

    pub region_order_user: Vec<String>,
    pub region_order_languages_user: Vec<String>,
    pub language_order_user: Vec<String>,
    pub localization_order_user: Vec<String>,
    pub video_order_user: Vec<String>,
    pub global_exclude: Vec<String>,
    pub global_include: Vec<String>,
    pub global_filter: Vec<String>,

    /// Whether the DAT's system has a settings file.
    pub system_found: bool,
    pub system_region_order_user: SystemOrder,
    /// The system's language list without its markers. Retool keeps it even when the system doesn't override the
    /// order, as the languages its regions imply, but reads it only with the marker.
    pub system_language_order_user: SystemOrder,
    pub system_languages_user_found: bool,
    pub system_localization_order_user: SystemOrder,
    pub system_video_order_user: SystemOrder,
    pub system_exclude: Vec<String>,
    pub system_include: Vec<String>,
    /// The system's post filters, and its override marker if it has one.
    pub system_filter: Vec<String>,
    pub system_filter_override: Option<bool>,

    pub options: Options,
}

impl Settings {
    /// The settings for a run of `profile` over a DAT whose system Retool names `search_name`.
    pub fn new(
        internal_config: &[u8],
        profile: &SelectionProfile,
        search_name: &str,
    ) -> Result<Settings, Error> {
        let internal: Internal = serde_json::from_slice(internal_config)
            .map_err(|e| Error::Install(format!("internal-config.json: {e}")))?;
        let languages = internal.languages;
        let mut languages_filter: HashMap<String, Vec<String>> = HashMap::new();
        let mut languages_implied: HashMap<String, Vec<String>> = HashMap::new();
        let strip_implied = Re::of(r"[^-\w].*");
        for (region, implied) in &internal.default_region_order {
            let mut filter = Vec::new();
            if let Some(pattern) = languages.get(&implied.implied_language) {
                filter.push(pattern.clone());
                languages_implied.insert(region.clone(), vec![strip_implied.sub("", pattern)]);
            }
            languages_filter.insert(region.clone(), filter);
        }
        let mut region_order_default: Vec<String> =
            internal.default_region_order.keys().cloned().collect();
        insert_united_kingdom(&mut region_order_default);
        if let Some(uk) = languages_implied.get("UK").cloned() {
            languages_implied.insert("United Kingdom".into(), uk);
            let filter = languages_filter.get("UK").cloned().unwrap_or_default();
            languages_filter.insert("United Kingdom".into(), filter);
        }
        let language_patterns: Vec<String> = languages.values().cloned().collect();
        let regex = Regexes::new(&language_patterns, &region_order_default);

        let tags_budget_editions = tags(&internal.budget_editions);
        let tags_promote_editions = tags(&internal.promote_editions);
        let tags_demote_editions = tags(&internal.demote_editions);
        let tags_modern_editions = tags(&internal.modern_editions);
        let mut tags_ignore = tags(&internal.ignore_tags);
        tags_ignore.extend(tags_budget_editions.iter().cloned());
        tags_ignore.extend(tags_promote_editions.iter().cloned());
        tags_ignore.extend(tags_demote_editions.iter().cloned());
        tags_ignore.extend(tags_modern_editions.iter().cloned());

        let default_regions: Vec<String> = internal.default_region_order.keys().cloned().collect();
        let mut region_order_user = if profile.region_order.is_empty() {
            default_regions.clone()
        } else {
            profile.region_order.clone()
        };
        let video_order_user = if profile.video_order.is_empty() {
            internal.default_video_order.clone()
        } else {
            profile.video_order.clone()
        };
        let mut region_order_languages_user: Vec<String> = Vec::new();
        for region in &region_order_user {
            for language in languages_filter.get(region).into_iter().flatten() {
                if !region_order_languages_user.contains(language) {
                    region_order_languages_user.push(language.clone());
                }
            }
        }
        let map_languages = |names: &[String]| -> Vec<String> {
            names
                .iter()
                .filter_map(|n| languages.get(n).cloned())
                .collect()
        };
        let language_order_user = if profile.language_order.is_empty() {
            region_order_languages_user.clone()
        } else {
            map_languages(&profile.language_order)
        };
        let localization_order_user = map_languages(&profile.localization_order);
        insert_united_kingdom(&mut region_order_user);

        let mut settings = Settings {
            regex,
            region_order_default,
            languages_filter,
            languages_implied,
            tags_ignore,
            tags_budget_editions,
            tags_promote_editions,
            tags_demote_editions,
            tags_modern_editions,
            tags_disc_rename: internal.disc_rename.into_iter().collect(),
            version_ignore: internal.version_ignore.iter().map(|p| Re::of(p)).collect(),
            dat_file_tags: internal.dat_file_tags,
            region_order_user,
            region_order_languages_user,
            language_order_user,
            localization_order_user,
            video_order_user,
            global_exclude: profile.overrides.exclude.clone(),
            global_include: profile.overrides.include.clone(),
            global_filter: profile.post_filters.clone(),
            system_found: false,
            system_region_order_user: None,
            system_language_order_user: None,
            system_languages_user_found: false,
            system_localization_order_user: None,
            system_video_order_user: None,
            system_exclude: Vec::new(),
            system_include: Vec::new(),
            system_filter: Vec::new(),
            system_filter_override: None,
            options: options(&profile.options, Exclusions::of(&profile.exclude)),
            languages,
        };
        if let Some(system) = profile.systems.get(search_name) {
            let selecting = SystemSettings {
                additions: false,
                ..system.clone()
            };
            // `Engine::run` writes no settings file for a system whose only setting is its Additions Set.
            if !(selecting.is_empty() && !system.is_empty()) {
                settings.apply_system(system, &default_regions, &internal.default_video_order);
            }
        }
        Ok(settings)
    }

    fn apply_system(
        &mut self,
        system: &SystemSettings,
        default_regions: &[String],
        default_videos: &[String],
    ) {
        self.system_found = true;
        self.system_region_order_user = system.region_order.as_ref().map(|order| {
            let mut order = if order.is_empty() {
                default_regions.to_vec()
            } else {
                order.clone()
            };
            insert_united_kingdom(&mut order);
            order
        });
        self.system_video_order_user = system.video_order.as_ref().map(|order| {
            if order.is_empty() {
                default_videos.to_vec()
            } else {
                order.clone()
            }
        });
        let implied = |s: &Settings| -> Vec<String> {
            let regions = s
                .system_region_order_user
                .as_ref()
                .unwrap_or(&s.region_order_user);
            let mut out: Vec<String> = Vec::new();
            for region in regions {
                for language in s.languages_filter.get(region).into_iter().flatten() {
                    if !out.contains(language) {
                        out.push(language.clone());
                    }
                }
            }
            out
        };
        self.system_language_order_user = match &system.language_order {
            Some(names) if names.is_empty() => Some(implied(self)),
            Some(names) => {
                self.system_languages_user_found = true;
                let mut out: Vec<String> = Vec::new();
                for name in names {
                    if let Some(pattern) = self.languages.get(name)
                        && !out.contains(pattern)
                    {
                        out.push(pattern.clone());
                    }
                }
                Some(out)
            }
            None => None,
        };
        self.system_localization_order_user = system.localization_order.as_ref().map(|names| {
            names
                .iter()
                .filter_map(|n| self.languages.get(n).cloned())
                .collect()
        });
        self.system_exclude = system.overrides.exclude.clone();
        self.system_include = system.overrides.include.clone();
        self.system_filter = system.post_filters.clone();
        self.system_filter_override = (system.replace_post_filters
            || !system.post_filters.is_empty())
        .then_some(system.replace_post_filters);
        if let Some(categories) = &system.exclude {
            self.options.exclude = Exclusions::of(categories);
        }
        if let Some(system_options) = &system.options {
            let exclude = self.options.exclude;
            self.options = options(system_options, exclude);
        }
    }

    /// The language order the language preference and filter read: the system's when it overrides it.
    pub fn system_language_override(&self) -> Option<&Vec<String>> {
        self.system_language_order_user.as_ref()
    }

    /// The region order: the system's when it overrides it.
    pub fn region_order(&self) -> &[String] {
        self.system_region_order_user
            .as_deref()
            .unwrap_or(&self.region_order_user)
    }
}

fn insert_united_kingdom(order: &mut Vec<String>) {
    if let Some(uk) = order.iter().position(|r| r == "UK") {
        order.insert(uk + 1, "United Kingdom".into());
    }
}
