//! The titles a run works on (`DatNode`), and the state a run carries between Retool's stages.

use std::collections::HashMap;

use indexmap::{IndexMap, IndexSet};

use super::dat::{Rom, TitleData};
use super::json::J;
use super::py::{Crash, PyResult};
use super::pyre::Re;
use super::pyset::PySet;
use super::settings::Settings;
use super::titletools::{self, Method, Versions};
use super::trace::Trace;

/// A title in the run's arena. Retool's titles are objects compared by identity; `copy.deepcopy` makes new ones.
pub(crate) type Id = usize;

/// A Python `set` of titles. CPython orders a set of objects by address, which no port can reproduce, so this
/// keeps insertion order.
pub(crate) type Titles = IndexSet<Id>;

/// Groups of titles by group name (`dict[str, set[DatNode]]`).
pub(crate) type Groups = IndexMap<String, Titles>;

/// A compilation's entry for a title it contains.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Contained {
    pub position: J,
    pub priority: J,
}

#[derive(Debug, Clone)]
pub(crate) struct Node {
    pub full_name: String,
    pub full_name_original: String,
    pub numbered_name: String,
    pub local_name: String,
    pub tags: PySet<String>,
    pub languages_implied: Vec<String>,
    pub languages_online: Vec<String>,
    pub languages_title: Vec<String>,
    pub languages: Vec<String>,
    pub languages_original: Vec<String>,
    pub regions: Vec<String>,
    pub primary_region: String,
    pub primary_region_original: String,
    pub secondary_region: String,
    pub short_name: String,
    pub short_name_original: String,
    pub region_free_name: String,
    pub group_name: String,
    pub group_name_conditional: String,
    pub group_moved_by_condition: bool,
    pub description: String,
    pub roms: Vec<Rom>,
    pub cloneof: String,
    pub clonelist_priority: i64,
    pub is_mia: bool,
    pub is_retroachievement: bool,
    pub is_oldest: bool,
    pub is_superset: bool,
    pub contains_titles: IndexMap<String, Contained>,
    pub categories: Vec<String>,
    pub is_demo: bool,
    pub exclude_reason: String,
    pub include_reason: String,
    pub exclude_include_related: bool,
    pub region_priority: i64,
    pub language_priority: i64,
    pub normalized_version: Versions,
}

/// The lists of titles Retool removes, by its `Removes` attribute names, in their definition order.
pub(crate) const REMOVES: [&str; 26] = [
    "add_ons_removes",
    "aftermarket_removes",
    "applications_removes",
    "audio_removes",
    "bad_dumps_removes",
    "bios_removes",
    "bonus_discs_removes",
    "coverdiscs_removes",
    "demos_removes",
    "educational_removes",
    "games_removes",
    "manuals_removes",
    "mia_removes",
    "multimedia_removes",
    "pirate_removes",
    "preproduction_removes",
    "promotional_removes",
    "unlicensed_removes",
    "video_removes",
    "clone_list_ignores",
    "language_removes",
    "region_removes",
    "system_excludes",
    "global_excludes",
    "global_filter_removes",
    "system_filter_removes",
];

pub(crate) struct Run {
    pub s: Settings,
    pub nodes: Vec<Node>,
    pub trace: Trace,
    pub removes: IndexMap<&'static str, Titles>,
    pub numbered: bool,
    patterns: HashMap<String, Re>,
}

impl Run {
    pub fn new(s: Settings) -> Run {
        Run {
            s,
            nodes: Vec::new(),
            trace: Trace::new(),
            removes: REMOVES.iter().map(|r| (*r, Titles::new())).collect(),
            numbered: false,
            patterns: HashMap::new(),
        }
    }

    pub fn n(&self, id: Id) -> &Node {
        &self.nodes[id]
    }

    pub fn m(&mut self, id: Id) -> &mut Node {
        &mut self.nodes[id]
    }

    pub fn add(&mut self, node: Node) -> Id {
        self.nodes.push(node);
        self.nodes.len() - 1
    }

    /// `copy.deepcopy` of one title.
    pub fn copy(&mut self, id: Id) -> Id {
        let node = self.nodes[id].clone();
        self.add(node)
    }

    /// `copy.deepcopy` of a set of titles.
    pub fn copy_set(&mut self, titles: &Titles) -> Titles {
        titles.iter().map(|&id| self.copy(id)).collect()
    }

    /// `copy.deepcopy` of groups: a title in two groups stays one title in the copy.
    pub fn copy_groups(&mut self, groups: &Groups) -> Groups {
        let mut memo: HashMap<Id, Id> = HashMap::new();
        let mut out = Groups::new();
        for (name, titles) in groups {
            let mut copied = Titles::new();
            for &id in titles {
                let new = match memo.get(&id) {
                    Some(&new) => new,
                    None => {
                        let new = self.copy(id);
                        memo.insert(id, new);
                        new
                    }
                };
                copied.insert(new);
            }
            out.insert(name.clone(), copied);
        }
        out
    }

    /// `re.search(pattern, text)` for a pattern Retool reads from its settings, compiled once.
    pub fn search(&mut self, pattern: &str, text: &str) -> bool {
        if !self.patterns.contains_key(pattern) {
            self.patterns.insert(pattern.to_owned(), Re::of(pattern));
        }
        self.patterns[pattern].search(text)
    }

    /// `re.search(pattern, text, flags=re.I)`, which raises on a pattern that doesn't compile.
    pub fn search_i(&mut self, pattern: &str, text: &str) -> PyResult<bool> {
        let key = format!("\u{0}i{pattern}");
        if !self.patterns.contains_key(&key) {
            let re = Re::new_i(pattern).map_err(|e| Crash(format!("re.error: {e}")))?;
            self.patterns.insert(key.clone(), re);
        }
        Ok(self.patterns[&key].search(text))
    }

    /// `TraceTools.trace_title`.
    pub fn trace_title<'a>(
        &mut self,
        reference: &str,
        variables: &[String],
        titles: impl IntoIterator<Item = &'a Id>,
        keep_remove: bool,
    ) {
        let titles: Vec<(String, String)> = titles
            .into_iter()
            .map(|&id| {
                (
                    self.nodes[id].full_name.clone(),
                    self.nodes[id].group_name.clone(),
                )
            })
            .collect();
        self.trace.step(reference, variables, &titles, keep_remove);
    }

    /// `trace_title` without titles.
    pub fn trace_ref(&mut self, reference: &str, variables: &[String]) {
        self.trace.step(reference, variables, &[], false);
    }

    /// `trace_title(reference, [keep, remove], keep_remove=True)`.
    pub fn trace_keep(&mut self, reference: &str, keep: &str, remove: &str) {
        self.trace
            .step(reference, &[keep.to_owned(), remove.to_owned()], &[], true);
    }

    /// `eprint(text)` into the trace.
    pub fn note(&mut self, text: &str) {
        self.trace.note(text);
    }

    pub fn remove_into(&mut self, list: &str, id: Id) {
        self.removes
            .get_mut(list)
            .unwrap_or_else(|| panic!("no list {list}"))
            .insert(id);
    }

    /// The region order the run's title selection reads.
    pub fn region_order(&self) -> Vec<String> {
        self.s.region_order().to_vec()
    }

    /// The language order a title's language priority reads.
    pub fn language_order(&self) -> Vec<String> {
        self.s
            .system_language_override()
            .cloned()
            .unwrap_or_else(|| self.s.language_order_user.clone())
    }

    /// `DatNode(config, input_dat, metadata, node)`.
    pub fn node(
        &mut self,
        numbered: bool,
        metadata: &IndexMap<String, J>,
        data: &TitleData,
    ) -> PyResult<Node> {
        let s = &self.s;
        let (mut full_name, numbered_name) = if numbered {
            (
                super::py::slice(&data.name, Some(7), None),
                data.name.clone(),
            )
        } else {
            (data.name.clone(), String::new())
        };
        full_name = titletools::replace_invalid_characters(&full_name, false);
        let tags: PySet<String> = full_name
            .split(" (")
            .skip(1)
            .map(|x| format!("({x}"))
            .collect();
        let entry = metadata.get(&full_name);
        let mut languages_online: Vec<String> = Vec::new();
        if let Some(entry) = entry {
            let languages = entry
                .get("languages")
                .ok_or_else(|| Crash("KeyError: 'languages'".into()))?;
            if languages.truthy() {
                let mut list: Vec<String> = match languages {
                    J::List(l) => l.iter().map(J::py_str).collect(),
                    J::Str(s) => s.chars().map(String::from).collect(),
                    J::Dict(d) => d.keys().cloned().collect(),
                    _ => return Err(Crash("TypeError: languages".into())),
                };
                list.sort();
                languages_online = list;
            }
        }
        let languages_title_orig_str = titletools::languages(&full_name, &tags, s, Method::Get);
        let languages_title_str = languages_title_orig_str.replace('+', ",");
        let mut languages_title: Vec<String> = Vec::new();
        if !languages_title_str.is_empty() {
            let mut set: Vec<String> = languages_title_str.split(',').map(str::to_owned).collect();
            set.sort();
            set.dedup();
            languages_title = set;
        }
        let regions_str = titletools::regions(&full_name, s, Method::Get);
        let mut regions: Vec<String> = s
            .region_order_default
            .iter()
            .filter(|r| regions_str.contains(r.as_str()))
            .cloned()
            .collect();
        if regions.is_empty() {
            regions = vec!["Unknown".into()];
        }
        let mut primary_region = regions[0].clone();
        let primary_region_original = primary_region.clone();
        let region_order = s.region_order().to_vec();
        if regions.len() > 1
            && let Some(region) = region_order.iter().find(|r| regions.contains(r))
        {
            primary_region = region.clone();
        }
        let secondary_region = if regions.len() > 1 {
            regions
                .iter()
                .find(|x| **x != primary_region)
                .cloned()
                .unwrap_or_default()
        } else {
            String::new()
        };
        let languages_implied = s
            .languages_implied
            .get(&primary_region)
            .cloned()
            .unwrap_or_default();
        let mut languages: Vec<String> = Vec::new();
        if !languages_online.is_empty() {
            if languages_online != ["nolang"] {
                languages = languages_online.clone();
            } else {
                languages_online.clear();
            }
        }
        if languages_online.is_empty() && languages_title.is_empty() {
            languages = languages_implied.clone();
        }
        if languages_online.is_empty() && !languages_title.is_empty() {
            languages = languages_title.clone();
        }
        let languages_original = languages.clone();
        if languages.iter().any(|l| l == "Zh") {
            let has = |r: &str| regions.iter().any(|x| x == r);
            if has("China") || has("Singapore") {
                languages = languages
                    .iter()
                    .map(|x| x.replace("Zh", "Zh-Hans"))
                    .collect();
            } else if has("Hong Kong") || has("Taiwan") {
                languages = languages
                    .iter()
                    .map(|x| x.replace("Zh", "Zh-Hant"))
                    .collect();
            }
        }
        let regions_replace = format!(" ({regions_str})");
        let languages_replace = format!(" ({languages_title_orig_str})");
        let short_name = titletools::get_short_name(&full_name, &tags, s);
        let region_free_name = full_name
            .replace(&languages_replace, "")
            .replace(&regions_replace, "");
        let group_name = titletools::get_group_name(&full_name, s);

        let mut local_name = String::new();
        if let Some(entry) = entry
            && let Some(local) = entry.get("localName")
        {
            let codes: Vec<String> = match &s.system_localization_order_user {
                Some(order) => order.clone(),
                None => s.localization_order_user.clone(),
            };
            let mut metadata_local_language = String::new();
            if let Some(filter) = s.languages_filter.get(&primary_region)
                && let Some(first) = filter.first()
            {
                metadata_local_language = first.clone();
            }
            if languages.len() <= 2
                && languages != ["En"]
                && codes.contains(&metadata_local_language)
            {
                local_name = local.py_str();
            }
        }
        if !local_name.is_empty() {
            let tail = Re::of(" \\(.*").find(&full_name);
            local_name = format!("{local_name}{tail}");
        }

        let mut roms = data.files.clone();
        let mia_name = full_name.contains("[MIA]");
        for rom in &mut roms {
            rom.crc = rom.crc.to_lowercase();
            rom.md5 = rom.md5.to_lowercase();
            rom.sha1 = rom.sha1.to_lowercase();
            rom.sha256 = rom.sha256.to_lowercase();
            rom.header = rom.header.to_lowercase();
            if mia_name {
                rom.mia = "yes".into();
            }
        }
        let is_mia = mia_name || roms.iter().filter(|r| r.mia == "yes").count() == roms.len();

        let mut categories: Vec<String> = data
            .categories
            .iter()
            .map(|c| {
                if c == "Console" {
                    "BIOS".into()
                } else {
                    c.clone()
                }
            })
            .collect();
        let r = &s.regex;
        let mut assign = |patterns: &[&Re], category: &str| {
            for pattern in patterns {
                if pattern.search(&full_name) && !categories.iter().any(|c| c == category) {
                    categories.push(category.into());
                }
            }
        };
        assign(&r.addons.iter().collect::<Vec<_>>(), "Add-Ons");
        assign(&[&r.programs], "Applications");
        assign(&[&r.audio], "Audio");
        assign(&[&r.bios], "BIOS");
        assign(&r.demos.iter().collect::<Vec<_>>(), "Demos");
        assign(&[&r.multimedia], "Multimedia");
        assign(&r.preproduction(), "Preproduction");
        assign(&r.video.iter().collect::<Vec<_>>(), "Video");
        if full_name.to_lowercase().contains("[bios]") {
            if !categories.iter().any(|c| c == "BIOS") {
                categories.push("BIOS".into());
            }
        } else if categories.is_empty() {
            categories.push("Games".into());
        }
        let is_demo = r.demos.iter().any(|d| d.search(&full_name))
            || (!r.demos.is_empty() && categories.iter().any(|c| c == "Demos"));

        let language_order = self.language_order();
        let mut region_priority: Option<i64> = None;
        let mut language_priority: Option<i64> = None;
        for (i, region) in region_order.iter().enumerate() {
            if regions.contains(region) {
                region_priority = Some(region_priority.map_or(i as i64, |p| p.min(i as i64)));
                for (j, code) in language_order.iter().enumerate() {
                    for language in &languages {
                        if self.search(code, language) {
                            language_priority =
                                Some(language_priority.map_or(j as i64, |p| p.min(j as i64)));
                        }
                    }
                }
            }
        }
        let s = &self.s;
        let normalized_version = titletools::get_normalized_version(&full_name, s)?;
        Ok(Node {
            full_name_original: full_name.clone(),
            numbered_name,
            local_name,
            tags,
            languages_implied,
            languages_online,
            languages_title,
            languages,
            languages_original,
            regions,
            primary_region_original,
            primary_region,
            secondary_region,
            short_name_original: short_name.clone(),
            short_name: short_name.to_lowercase(),
            region_free_name,
            group_name,
            group_name_conditional: String::new(),
            group_moved_by_condition: false,
            description: data.description.clone(),
            roms,
            cloneof: String::new(),
            clonelist_priority: 1,
            is_mia,
            is_retroachievement: false,
            is_oldest: false,
            is_superset: false,
            contains_titles: IndexMap::new(),
            categories,
            is_demo,
            exclude_reason: String::new(),
            include_reason: String::new(),
            exclude_include_related: false,
            region_priority: region_priority.unwrap_or(100),
            language_priority: language_priority.unwrap_or(100),
            normalized_version,
            full_name,
        })
    }
}
