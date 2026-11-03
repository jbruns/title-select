//! The system each DAT is for, named as Retool names it: the key a Profile's system settings and choices are
//! filed under. Retool derives the name from the DAT's header with tags from its own config, so the engine's
//! Python asks Retool rather than this crate copying its rules.

use std::io::Write as _;
use std::path::Path;
use std::process::{Command, Stdio};

use crate::Error;

/// Reads a JSON list of DAT paths on stdin and prints, per DAT, Retool's system name or `null`. Retool's own
/// notes go to stderr, so that stdout carries only the answer.
const NAME: &str = r#"
import json, os, pathlib, sys
retool, user_config = sys.argv[1], sys.argv[2]
dats = json.load(sys.stdin)
answer = sys.stdout
sys.stdout = sys.stderr
sys.path.insert(0, retool)
import modules.constants as const
from modules.config.config import Config
from modules.dat.parse_dat import get_dat_header
from modules.dat.process_dat import Dat
from modules.input import check_input
sys.argv = [os.path.join(retool, 'retool.py'), dats[0], '--config', user_config, '--system-config-dir', 'systems']
config = Config(
    const.CLONE_LIST_METADATA_DOWNLOAD_LOCATION, const.CLONE_LIST_METADATA_DOWNLOAD_LOCATION_KEY,
    const.PROGRAM_DOWNLOAD_LOCATION, const.PROGRAM_DOWNLOAD_LOCATION_KEY, const.CONFIG_FILE,
    const.DAT_FILE_TAGS_KEY, const.IGNORE_TAGS_KEY, const.DISC_RENAME_KEY, const.VERSION_IGNORE_KEY,
    const.BUDGET_EDITIONS_KEY, const.PROMOTE_EDITIONS_KEY, const.DEMOTE_EDITIONS_KEY,
    const.MODERN_EDITIONS_KEY, const.LANGUAGES_KEY, const.REGION_ORDER_KEY, const.VIDEO_ORDER_KEY,
    const.CLONE_LISTS_KEY, const.METADATA_KEY, const.MIAS_KEY, const.RA_KEY, const.USER_CONFIG_KEY,
    const.USER_LANGUAGE_ORDER_KEY, const.USER_REGION_ORDER_KEY, const.USER_LOCALIZATION_ORDER_KEY,
    const.USER_VIDEO_ORDER_KEY, const.USER_LIST_PREFIX_KEY, const.USER_LIST_SUFFIX_KEY,
    const.USER_OVERRIDE_EXCLUDE_KEY, const.USER_OVERRIDE_INCLUDE_KEY, const.USER_FILTER_KEY,
    const.USER_GUI_SETTINGS_KEY, const.SYSTEM_SETTINGS_PATH, const.SANITIZED_CHARACTERS,
    const.RESERVED_FILENAMES, check_input(),
)
names = []
for dat in dats:
    try:
        names.append(get_dat_header(pathlib.Path(dat), Dat(), config).search_name or None)
    except BaseException:
        names.append(None)
json.dump(names, answer)
"#;

/// Names the system of each DAT in `dats`, or `None` for one Retool can't read, running in `run_dir`.
pub(crate) fn name(
    python: &Path,
    retool: &Path,
    run_dir: &Path,
    user_config: &Path,
    dats: &[&Path],
) -> Result<Vec<Option<String>>, Error> {
    if dats.is_empty() {
        return Ok(Vec::new());
    }
    let context = || format!("naming systems with {}", python.display());
    let io = |source| Error::Io {
        context: context(),
        source,
    };
    std::fs::create_dir_all(run_dir.join("systems")).map_err(io)?;
    let mut child = Command::new(python)
        .args(["-I", "-c", NAME])
        .arg(retool)
        .arg(user_config)
        .current_dir(run_dir)
        .env_clear()
        .envs([
            ("PYTHONDONTWRITEBYTECODE", "1"),
            ("PYTHONIOENCODING", "utf-8"),
            ("LANG", "C.UTF-8"),
        ])
        .env("HOME", run_dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(io)?;
    let mut stdin = child.stdin.take().expect("stdin is piped");
    stdin
        .write_all(&serde_json::to_vec(dats).expect("paths serialize"))
        .map_err(io)?;
    drop(stdin);
    let output = child.wait_with_output().map_err(io)?;
    let names: Vec<Option<String>> = match output.status.success() {
        true => serde_json::from_slice(&output.stdout).ok(),
        false => None,
    }
    .filter(|names: &Vec<Option<String>>| names.len() == dats.len())
    .ok_or_else(|| {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let lines: Vec<&str> = stderr.lines().filter(|l| !l.trim().is_empty()).collect();
        Error::Install(format!(
            "{} failed: {}",
            context(),
            lines[lines.len().saturating_sub(5)..].join("\n")
        ))
    })?;
    Ok(names)
}
