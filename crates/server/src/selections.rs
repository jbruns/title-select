//! Running the selection engine for the Service: Selection Jobs and live previews, each on a copy of the DAT
//! Revision's blob named as it was imported, in a scratch folder under the store's `tmp`.

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use title_select_api::selection::{Selection, SelectionEngine, SelectionProfile, ShadowRun};
use title_select_selection::{Engine, Error, RunOptions};
use title_select_store::{Store, TMP};

/// Why a Selection or preview cannot run without an engine install.
pub const UNAVAILABLE: &str = "the selection engine is not installed on this server, so selections and previews \
    are unavailable; start the server with --retool, or set TITLE_SELECT_RETOOL";

/// Which engine runs, and whether the other runs in its shadow.
#[derive(Debug, Clone, Copy, Default)]
pub struct Engines {
    pub engine: SelectionEngine,
    pub shadow: bool,
}

/// Runs the engine on the DAT blob `blob`, staged under its imported file name `file`, with the shadow run when
/// `engines` asks for one.
pub fn select(
    store: &Store,
    engine: &Engine,
    blob: &Path,
    file: &str,
    profile: &SelectionProfile,
    engines: Engines,
    cancel: Option<Arc<AtomicBool>>,
) -> Result<(Selection, Option<ShadowRun>), Error> {
    let work = store.dir().join(TMP);
    let io = |context: String| move |source| Error::Io { context, source };
    std::fs::create_dir_all(&work).map_err(io(work.display().to_string()))?;
    let staged = tempfile::Builder::new()
        .prefix("dat-")
        .tempdir_in(&work)
        .map_err(io(work.display().to_string()))?;
    let name = Path::new(file)
        .file_name()
        .filter(|n| !n.is_empty())
        .unwrap_or(std::ffi::OsStr::new("input.dat"));
    let dat = staged.path().join(name);
    if std::fs::hard_link(blob, &dat).is_err() {
        std::fs::copy(blob, &dat).map_err(io(blob.display().to_string()))?;
    }
    let mut options = RunOptions::new(&work);
    options.cancel = cancel;
    options.engine = engines.engine;
    options.shadow = engines.shadow;
    let outcome = engine.run(&dat, profile, &options)?;
    for warning in &outcome.warnings {
        eprintln!("title-select: selection engine warning for {file}: {warning}");
    }
    if let Some(shadow) = &outcome.shadow {
        eprintln!("title-select: {}", shadow_summary(file, shadow));
    }
    drop(staged);
    Ok((outcome.selection, outcome.shadow))
}

/// One line on how a shadow run compared, for the Service log.
pub fn shadow_summary(file: &str, shadow: &ShadowRun) -> String {
    let engines = format!(
        "{} engine in the shadow of the {} engine",
        engine_name(shadow.shadow),
        engine_name(shadow.engine)
    );
    match (&shadow.comparison, &shadow.error) {
        (_, Some(error)) => format!("shadow run for {file}: the {engines} failed: {error}"),
        (Some(c), None) if c.is_identical() => {
            format!(
                "shadow run for {file}: the {engines} made the same selection in all {} groups",
                c.groups
            )
        }
        (Some(c), None) => {
            let mut groups: Vec<&str> = c
                .differences
                .iter()
                .filter(|d| !d.explanation_only)
                .map(|d| d.group.as_str())
                .take(10)
                .collect();
            if c.differing > groups.len() {
                groups.push("…");
            }
            format!(
                "shadow run for {file}: the {engines} placed releases differently in {} of {} groups{}, explained {} \
                 more differently{}. These differences are unclassified: some may only reflect the order the bundled \
                 engine meets titles in. Run `title-select conformance` on {file} to classify them",
                c.differing,
                c.groups,
                if groups.is_empty() {
                    String::new()
                } else {
                    format!(" ({})", groups.join(", "))
                },
                c.explained_differently,
                if c.warnings.is_some() {
                    ", and warned differently"
                } else {
                    ""
                },
            )
        }
        (None, None) => format!("shadow run for {file}: the {engines} made no comparison"),
    }
}

fn engine_name(engine: SelectionEngine) -> &'static str {
    match engine {
        SelectionEngine::Bundled => "bundled",
        SelectionEngine::Builtin => "built-in",
    }
}

/// The preview each actor has in flight. A newer preview from the same actor cancels the older one.
#[derive(Default)]
pub struct Previews(Mutex<HashMap<String, Arc<AtomicBool>>>);

impl Previews {
    /// Registers a new preview for `actor`, cancelling the one in flight, if any.
    pub fn start(self: &Arc<Self>, actor: &str) -> PreviewGuard {
        let flag = Arc::new(AtomicBool::new(false));
        if let Some(older) = self
            .0
            .lock()
            .unwrap()
            .insert(actor.to_owned(), flag.clone())
        {
            older.store(true, Ordering::Relaxed);
        }
        PreviewGuard {
            previews: self.clone(),
            actor: actor.to_owned(),
            flag,
        }
    }
}

/// Cancels its preview when dropped: when the preview ends, or when the client went away and the request was
/// dropped.
pub struct PreviewGuard {
    previews: Arc<Previews>,
    actor: String,
    flag: Arc<AtomicBool>,
}

impl PreviewGuard {
    pub fn flag(&self) -> Arc<AtomicBool> {
        self.flag.clone()
    }
}

impl Drop for PreviewGuard {
    fn drop(&mut self) {
        self.flag.store(true, Ordering::Relaxed);
        let mut previews = self.previews.0.lock().unwrap();
        if previews
            .get(&self.actor)
            .is_some_and(|f| Arc::ptr_eq(f, &self.flag))
        {
            previews.remove(&self.actor);
        }
    }
}

/// The systems already named, by DAT Revision SHA-256: a revision's header never changes.
static SYSTEMS: Mutex<Option<HashMap<String, Option<String>>>> = Mutex::new(None);

/// The system each registered Source Set's latest DAT Revision is for, asking the engine only about revisions
/// it hasn't named yet. A Source Set without a readable revision has none.
pub fn systems(
    store: &Store,
    engine: &Engine,
) -> Result<Vec<title_select_api::SourceSetSystem>, crate::routes::ApiError> {
    let revisions: Vec<(String, Option<(String, std::path::PathBuf)>)> = store
        .source_sets()?
        .into_iter()
        .map(|set| {
            let revision = store
                .selection_revision(&set.name, None)
                .ok()
                .map(|(revision, blob)| (revision.sha256, blob));
            (set.name, revision)
        })
        .collect();
    let unnamed: Vec<&(String, std::path::PathBuf)> = {
        let known = SYSTEMS.lock().unwrap();
        let mut seen = std::collections::HashSet::new();
        revisions
            .iter()
            .filter_map(|(_, r)| r.as_ref())
            .filter(|(sha, _)| known.as_ref().is_none_or(|k| !k.contains_key(sha)))
            .filter(|(sha, _)| seen.insert(sha.clone()))
            .collect()
    };
    if !unnamed.is_empty() {
        let dats: Vec<&Path> = unnamed.iter().map(|(_, blob)| blob.as_path()).collect();
        let names = engine.systems(&dats, &store.dir().join(TMP))?;
        let mut known = SYSTEMS.lock().unwrap();
        let known = known.get_or_insert_with(HashMap::new);
        for ((sha, _), name) in unnamed.into_iter().zip(names) {
            known.insert(sha.clone(), name);
        }
    }
    let known = SYSTEMS.lock().unwrap();
    Ok(revisions
        .into_iter()
        .map(|(source_set, revision)| title_select_api::SourceSetSystem {
            source_set,
            system: revision
                .and_then(|(sha, _)| known.as_ref().and_then(|k| k.get(&sha).cloned().flatten())),
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_newer_preview_cancels_the_older_one_of_the_same_actor_only() {
        let previews = Arc::new(Previews::default());
        let older = previews.start("someone");
        let other = previews.start("someone else");
        let newer = previews.start("someone");

        assert!(older.flag().load(Ordering::Relaxed));
        assert!(!newer.flag().load(Ordering::Relaxed));
        assert!(!other.flag().load(Ordering::Relaxed));
        drop(older);
        assert!(
            previews.0.lock().unwrap().contains_key("someone"),
            "the older guard leaves the newer preview registered"
        );
        let flag = newer.flag();
        drop(newer);
        assert!(
            flag.load(Ordering::Relaxed),
            "a dropped request kills its run"
        );
        assert!(!previews.0.lock().unwrap().contains_key("someone"));
    }

    #[test]
    fn a_shadow_runs_log_line_marks_its_differences_unclassified() {
        use title_select_api::selection::EngineComparison;
        let run = |warnings| {
            let comparison = EngineComparison {
                groups: 3,
                warnings,
                ..EngineComparison::default()
            };
            shadow_summary(
                "Test.dat",
                &ShadowRun::new(
                    SelectionEngine::Bundled,
                    SelectionEngine::Builtin,
                    Some(comparison),
                    None,
                ),
            )
        };
        let differing = run(Some([vec!["a".into()], vec![]]));
        assert!(differing.contains("unclassified"), "{differing}");
        assert!(
            differing.contains("`title-select conformance` on Test.dat"),
            "{differing}"
        );
        let alike = run(None);
        assert!(!alike.contains("unclassified"), "{alike}");
    }
}
