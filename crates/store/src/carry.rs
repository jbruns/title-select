//! Carrying a file into a folder that no rename reaches, on another filesystem (ADR 0023): a Placed Game's file a
//! repair keeps aside into its Source Set's kept files, or a file a repair wrote put in place in the Library Folder
//! holding its Game. Builds that place Games carry them the same way, and delete the Source Set's files a Game was
//! converted from, or that a file built from them stands in for, as [`dispose_as`].
//!
//! The copy is made under a staged name recorded first, read back and checked against the SHA-256 the original was
//! read as while it held still, and published by a rename that never replaces anything. Only then is the original
//! deleted, under the trusted-stamp rule (ADR 0010): moved into its folder's private `.deleting` area under a slot
//! recorded first, and deleted there only while it still has the change time read through the descriptor held across
//! that rename, and that SHA-256, and while the published copy, held open throughout, is still the copy at its name
//! and still reads as that SHA-256. Otherwise it is put back where it was and left. Each step is recorded through
//! [`Intents`] before it is taken, so a stop resumes from where it was.

use std::fs::File;
use std::io;

use rustix::fs::{FileType, Stat};
use title_select_api::build::FileStat;
use title_select_engine::meter::{Tally, copy_chunked};

use crate::builds::{sha256_of, still_sha256};
use crate::folder_dir::{FolderDir, Identity, create_staged, ctime_ns, file_stat, staged_name};
use crate::kept::unique;
use crate::{Error, Result};

/// The file carried.
pub(crate) struct Original<'a> {
    /// The folder it is in, and its name there.
    pub dir: &'a FolderDir,
    pub name: &'a str,
    /// How it is named to you.
    pub label: &'a str,
    /// The file it must still be: its identity, size and modification time, and its change time when that is known.
    pub id: Identity,
    pub file: FileStat,
    pub ctime: Option<i128>,
    /// What it must read as, when that is known already: a file Title Select wrote.
    pub sha256: Option<&'a str>,
    /// Its folder's private `.deleting` area, on its filesystem.
    pub deleting: &'a FolderDir,
    /// Where what is read of it is counted.
    pub tally: &'a Tally,
}

/// Where it is carried.
pub(crate) struct Dest<'a> {
    /// The folder Title Select manages that the copy is staged in, on the destination's filesystem.
    pub root: &'a FolderDir,
    /// The folder it is put in, and its name there.
    pub dir: &'a FolderDir,
    pub name: &'a str,
    /// Where what is written of it, and read back, is counted.
    pub tally: &'a Tally,
}

/// What a carry has recorded of itself, each part before what it records is done.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Intent {
    /// The name the copy is staged under, in [`Dest::root`], and its identity once it is made.
    pub staged: Option<String>,
    pub staged_id: Option<Identity>,
    /// The SHA-256 the original was read as, and its copy read back as, before the copy was published.
    pub sha256: Option<String>,
    /// The original's slot in its folder's `.deleting` area, and its change time once moved there, read through the
    /// descriptor held across the rename.
    pub slot: Option<String>,
    pub moved_ctime: Option<i128>,
}

/// Where a carry records its [`Intent`], durably.
pub(crate) trait Intents {
    fn load(&self) -> Result<Intent>;
    fn save(&self, intent: &Intent) -> Result<()>;
    /// Called once the original is moved into its `.deleting` slot, before its change time there is recorded: a point a
    /// test can stop at.
    fn moved(&self) {}
    /// Called once the original is set aside to delete and read back, just before its published copy is checked a
    /// last time: a point a test can change the copy at.
    fn verifying(&self) {}
    /// Whether the folder the copy was published in is still the one at its registered path, checked last, once the
    /// copy holds: why not, otherwise, and the original is then not deleted.
    fn in_place(&self) -> Result<std::result::Result<(), String>> {
        Ok(Ok(()))
    }
    /// Takes the original, set aside in `slot` of its `.deleting` folder and not proven unchanged, somewhere other
    /// than back where it was, saying where; `None` puts it back. `Err` leaves it in its slot, recorded, for recovery.
    fn keep_instead(
        &self,
        _o: &Original,
        _slot: &str,
        _why: &str,
    ) -> Result<Option<std::result::Result<String, String>>> {
        Ok(None)
    }
}

fn io_err(e: io::Error) -> Error {
    Error::Io(e.to_string())
}

fn is_file(st: &Stat, id: Identity) -> bool {
    FolderDir::kind(st) == FileType::RegularFile && Identity::of(st) == id
}

/// Copies `o` to `d` and publishes the copy, leaving `o` where it is. Returns the published copy, as it is; or why
/// nothing was published, with nothing of the copy left behind.
pub(crate) fn copy(
    o: &Original,
    d: &Dest,
    intents: &dyn Intents,
) -> Result<std::result::Result<Stat, String>> {
    let mut intent = intents.load()?;
    // Published before a stop: the copy recorded, checked, is at its name.
    if let (Some(id), Some(_)) = (intent.staged_id, &intent.sha256)
        && let Some(st) = d.dir.stat(d.name).map_err(io_err)?
        && is_file(&st, id)
    {
        return Ok(Ok(st));
    }
    // A copy a stop left unpublished is removed, only while it is the file recorded, and made again.
    if let Some(staged) = &intent.staged {
        match (d.root.stat(staged).map_err(io_err)?, intent.staged_id) {
            (None, _) => {}
            (Some(st), Some(id)) if is_file(&st, id) => {
                d.root.unlink(staged).map_err(io_err)?;
                d.root.sync_holding(staged).map_err(io_err)?;
            }
            (Some(_), _) => {
                return Ok(Err(format!(
                    "{}/{staged} is not the copy Title Select was making of {}; it was left as it is",
                    d.root.path().display(),
                    o.label
                )));
            }
        }
    }
    let leaf = d.name.rsplit('/').next().unwrap_or(d.name);
    let staged = staged_name(leaf);
    intent = Intent {
        staged: Some(staged.clone()),
        ..Intent::default()
    };
    intents.save(&intent)?;
    let out = match create_staged(d.root, &staged) {
        Ok(out) => out,
        Err(e) => {
            intents.save(&Intent::default())?;
            return Ok(Err(format!("{} could not be copied: {e}", o.label)));
        }
    };
    let id = Identity::of_fd(&out).map_err(io_err)?;
    intent.staged_id = Some(id);
    intents.save(&intent)?;
    let checked = (|| -> Result<std::result::Result<String, String>> {
        let src = match o.dir.open_file(o.name) {
            Ok(src) => src,
            Err(e) => return Ok(Err(format!("{} could not be read: {e}", o.label))),
        };
        let st = rustix::fs::fstat(&src).map_err(|e| io_err(e.into()))?;
        let ctime = o.ctime.unwrap_or(ctime_ns(&st));
        if Identity::of(&st) != o.id || file_stat(&st) != o.file || ctime_ns(&st) != ctime {
            return Ok(Err(format!("{} changed since the plan was made", o.label)));
        }
        if let Err(e) = copy_chunked(&src, o.tally, &out, d.tally) {
            return Ok(Err(format!("{} could not be copied: {e}", o.label)));
        }
        out.sync_all().map_err(io_err)?;
        let Some(sha) = still_sha256(&src, ctime, o.tally).map_err(io_err)? else {
            return Ok(Err(format!("{} changed while it was copied", o.label)));
        };
        if o.sha256.is_some_and(|want| want != sha) {
            return Ok(Err(format!(
                "{} no longer reads as the file Title Select wrote",
                o.label
            )));
        }
        let (copied, _) = sha256_of(&out, d.tally).map_err(io_err)?;
        if copied != sha {
            return Ok(Err(format!(
                "the copy of {} didn't read back as it",
                o.label
            )));
        }
        Ok(Ok(sha))
    })()?;
    let discard = |why: String| -> Result<std::result::Result<Stat, String>> {
        if let Some(st) = d.root.stat(&staged).map_err(io_err)?
            && is_file(&st, id)
        {
            d.root.unlink(&staged).map_err(io_err)?;
            d.root.sync_holding(&staged).map_err(io_err)?;
        }
        intents.save(&Intent::default())?;
        Ok(Err(why))
    };
    let sha = match checked {
        Ok(sha) => sha,
        Err(why) => return discard(why),
    };
    intent.sha256 = Some(sha);
    intents.save(&intent)?;
    d.root.sync_holding(&staged).map_err(io_err)?;
    let _pin = d.root.pin(&staged, id);
    if let Err(e) = d.root.rename_new(&staged, d.dir, d.name) {
        return discard(format!(
            "the copy of {} could not be put in place: {e}",
            o.label
        ));
    }
    d.dir.sync().map_err(io_err)?;
    d.root.sync_holding(&staged).map_err(io_err)?;
    match d.dir.stat(d.name).map_err(io_err)? {
        Some(st) if is_file(&st, id) => Ok(Ok(st)),
        _ => Ok(Err(format!(
            "what is in place of the copy of {} is not the copy Title Select made",
            o.label
        ))),
    }
}

/// A published copy, opened without following a symlink and held, with its stat as it was opened: what is checked
/// once more, last, before the original it stands in for is deleted. A carry's ([`dispose`]) and an Ingest Folder
/// move's alike.
pub(crate) struct Held {
    file: File,
    st: Stat,
}

impl Held {
    /// Opens `name` in `dir`, while it is the file `id`, to hold it. Returns why not.
    pub(crate) fn open(
        dir: &FolderDir,
        name: &str,
        id: Identity,
    ) -> Result<std::result::Result<Held, String>> {
        let file = match dir.open_file(name) {
            Ok(file) => file,
            Err(e) => return Ok(Err(format!("its copy can't be read ({e})"))),
        };
        let st = rustix::fs::fstat(&file).map_err(|e| io_err(e.into()))?;
        if Identity::of(&st) != id {
            return Ok(Err(
                "what is in place of its copy is not the copy Title Select made".into(),
            ));
        }
        Ok(Ok(Held { file, st }))
    }

    /// Whether this is still the file at `name` in `dir`, unchanged since it was opened, and reads, through its own
    /// descriptor, as the SHA-256 `sha` recorded, counted to `tally`. Returns why not.
    ///
    /// Checked as the last step before an original is deleted. A change to the copy after this check can't be stopped
    /// by any lock; it is as any later change to a kept or placed file, which the next audit or check finds.
    pub(crate) fn still(
        &self,
        dir: &FolderDir,
        name: &str,
        sha: &str,
        tally: &Tally,
    ) -> Result<std::result::Result<(), String>> {
        match dir.stat(name).map_err(io_err)? {
            Some(st) if is_file(&st, Identity::of(&self.st)) => {}
            _ => {
                return Ok(Err(
                    "what is in place of its copy is not the copy Title Select made".into(),
                ));
            }
        }
        let now = rustix::fs::fstat(&self.file).map_err(|e| io_err(e.into()))?;
        if file_stat(&now) != file_stat(&self.st) {
            return Ok(Err("its copy changed since it was published".into()));
        }
        Ok(
            match still_sha256(&self.file, ctime_ns(&self.st), tally).map_err(io_err)? {
                Some(read) if read == sha => Ok(()),
                _ => Err("its copy changed since it was published".into()),
            },
        )
    }
}

/// Opens the copy [`copy`] published as `d`, while it is the copy it made, to hold it. Returns why not.
fn open_published(d: &Dest, intent: &Intent) -> Result<std::result::Result<Held, String>> {
    match intent.staged_id {
        Some(id) => Held::open(d.dir, d.name, id),
        None => Ok(Err("no copy of it is recorded".into())),
    }
}

/// Whether `held` is still the copy at `d`'s name, as [`Held::still`], reading as `sha`.
fn still_holds(d: &Dest, held: &Held, sha: &str) -> Result<std::result::Result<(), String>> {
    held.still(d.dir, d.name, sha, d.tally)
}

/// Whether the copy [`copy`] published as `d` is still the copy it made, reading as the SHA-256 recorded: so that a
/// copy changed since it was published, before a stop or after, never stands in for the original. Returns why not.
pub(crate) fn published_holds(
    d: &Dest,
    intents: &dyn Intents,
) -> Result<std::result::Result<(), String>> {
    published_holds_as(d, intents, None)
}

/// As [`published_holds`], for a published file that reads as `published`, when it is not a copy of the original
/// but converted from it.
fn published_holds_as(
    d: &Dest,
    intents: &dyn Intents,
    published: Option<&str>,
) -> Result<std::result::Result<(), String>> {
    let intent = intents.load()?;
    let Some(sha) = &intent.sha256 else {
        return Ok(Err("no copy of it is recorded".into()));
    };
    Ok(match open_published(d, &intent)? {
        Ok(held) => still_holds(d, &held, published.unwrap_or(sha))?,
        Err(why) => Err(why),
    })
}

/// Deletes `o` once [`copy`] published its copy as `d`: only while `o` is still exactly what was copied, read through
/// the descriptor held as it is moved to be deleted, and then, last, that copy, held open from before `o` is moved,
/// still holds ([`still_holds`]). Otherwise it is put back, and the reason it was left is returned.
pub(crate) fn dispose(
    o: &Original,
    d: &Dest,
    intents: &dyn Intents,
) -> Result<std::result::Result<(), String>> {
    dispose_as(o, d, intents, None)
}

/// As [`dispose`], for an original that `d`, published as the file [`Intent::staged_id`] names, stands in for though it
/// is not a copy of it: a file converted from it, or built from the same ROMs, which reads as `published`. `o` must
/// still read as [`Intent::sha256`], and `d` as `published`, for `o` to be deleted.
pub(crate) fn dispose_as(
    o: &Original,
    d: &Dest,
    intents: &dyn Intents,
    published: Option<&str>,
) -> Result<std::result::Result<(), String>> {
    let mut intent = intents.load()?;
    let Some(sha) = intent.sha256.clone() else {
        return Ok(Err(format!(
            "{} has no copy recorded, so it was left as it is",
            o.label
        )));
    };
    // Held from before the original is touched until it is deleted, so that the copy checked last is the one held.
    let held = open_published(d, &intent)?;
    let unslot = |intent: &mut Intent| -> Result<()> {
        intent.slot = None;
        intent.moved_ctime = None;
        intents.save(intent)
    };
    let slot = match intent.slot.clone() {
        Some(slot) => slot,
        None => {
            match o.dir.stat(o.name).map_err(io_err)? {
                // Gone already, by another hand: nothing is left to delete, while its copy holds.
                None => {
                    return Ok(published_holds_as(d, intents, published)?
                        .map_err(|why| format!("{} is gone, and {why}", o.label)));
                }
                Some(_) if let Err(why) = &held => {
                    return Ok(Err(format!(
                        "{} was not deleted, since {why}, so it was left as it is",
                        o.label
                    )));
                }
                Some(st) if is_file(&st, o.id) && file_stat(&st) == o.file => {}
                Some(_) => {
                    return Ok(Err(format!(
                        "{} changed after it was copied, so it was left as it is",
                        o.label
                    )));
                }
            }
            let slot = format!("carried-{}", unique());
            intent.slot = Some(slot.clone());
            intent.moved_ctime = None;
            intents.save(&intent)?;
            let pin = o.dir.pin(o.name, o.id);
            if pin.is_none() {
                unslot(&mut intent)?;
                return Ok(Err(format!(
                    "{} changed after it was copied, so it was left as it is",
                    o.label
                )));
            }
            if let Err(e) = o.dir.rename_new(o.name, o.deleting, &slot) {
                unslot(&mut intent)?;
                return Ok(Err(format!(
                    "{} could not be set aside to delete ({e}), so it was left as it is",
                    o.label
                )));
            }
            intents.moved();
            // Its change time, which the rename moved, read through the descriptor held across it.
            if let Some(held) = &pin {
                let moved = rustix::fs::fstat(held).map_err(|e| io_err(e.into()))?;
                if Identity::of(&moved) == o.id {
                    intent.moved_ctime = Some(ctime_ns(&moved));
                    intents.save(&intent)?;
                }
            }
            slot
        }
    };
    // Whether this call or one before a stop moved it, both folders are synced before it is deleted.
    o.dir.sync().map_err(io_err)?;
    o.deleting.sync().map_err(io_err)?;
    let Some(st) = o.deleting.stat(&slot).map_err(io_err)? else {
        // Deleted before a stop; or never moved, when it is still where it was.
        let still = o
            .dir
            .stat(o.name)
            .map_err(io_err)?
            .is_some_and(|st| is_file(&st, o.id));
        unslot(&mut intent)?;
        return match still {
            true => dispose_as(o, d, intents, published),
            false => Ok(Ok(())),
        };
    };
    let unchanged = match intent.moved_ctime {
        Some(ctime) if is_file(&st, o.id) => {
            let file = o.deleting.open_file(&slot).map_err(io_err)?;
            let held = rustix::fs::fstat(&file).map_err(|e| io_err(e.into()))?;
            Identity::of(&held) == o.id
                && file_stat(&held) == o.file
                && still_sha256(&file, ctime, o.tally)
                    .map_err(io_err)?
                    .is_some_and(|read| read == sha)
        }
        _ => false,
    };
    // Deleted only while its copy holds: checked last, through the descriptor held since before the original was
    // moved, once nothing but the unlink is left. A change to the copy after this check can't be stopped by any lock;
    // it is as any later change to a kept or placed file, which the next audit or check finds.
    let holds = match (&held, unchanged) {
        (Ok(held), true) => {
            intents.verifying();
            match still_holds(d, held, published.unwrap_or(&sha))? {
                Ok(()) => intents.in_place()?,
                Err(why) => Err(why),
            }
        }
        (Ok(_), false) => Ok(()),
        (Err(why), _) => Err(why.clone()),
    };
    if unchanged && holds.is_ok() {
        o.deleting.unlink(&slot).map_err(io_err)?;
        o.deleting.sync().map_err(io_err)?;
        unslot(&mut intent)?;
        return Ok(Ok(()));
    }
    // Never deleted unproven: kept, or put back where it was.
    let why = match holds {
        Err(why) => format!("{} was not deleted, since {why}", o.label),
        Ok(()) => format!(
            "{} could not be confirmed unchanged once it was set aside to delete",
            o.label
        ),
    };
    if let Some(kept) = intents.keep_instead(o, &slot, &why)? {
        return match kept {
            Ok(kept) => {
                unslot(&mut intent)?;
                Ok(Err(format!("{why}, so it was {kept}")))
            }
            Err(e) => Ok(Err(format!(
                "{why}, and could not be kept ({e}); it was left at .title-select/retained/.deleting/{slot}"
            ))),
        };
    }
    match o.deleting.rename_new(&slot, o.dir, o.name) {
        Ok(()) => {
            o.deleting.sync().map_err(io_err)?;
            o.dir.sync().map_err(io_err)?;
            unslot(&mut intent)?;
            Ok(Err(format!("{why}, so it was put back and left as it is")))
        }
        Err(e) => Ok(Err(format!(
            "{why}, and could not be put back ({e}); it was left at .title-select/retained/.deleting/{slot}"
        ))),
    }
}
