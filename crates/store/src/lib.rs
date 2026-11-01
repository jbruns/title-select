//! The store (ADR 0003): one SQLite database in a store directory, with DAT blobs under `dats/<sha256>`.

mod audits;
mod blobs;
mod builds;
mod checks;
pub mod config;
mod dats;
mod folder_dir;
mod folders;
mod ingest;
mod jobs;
mod kept;
mod migrations;
mod plans;
mod profiles;
mod selections;
mod sets;

use std::fs::{File, OpenOptions, TryLockError};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use rusqlite::{Connection, OpenFlags, Transaction, TransactionBehavior};

pub use audits::StoredAudit;
pub use ingest::OwedAudits;
pub use jobs::Interruption;
pub use selections::SelectionInput;
pub use title_select_api::{
    AuditSummary, DatRevisionRecord, ImportOutcome, Job, JobState, LastVerified, SourceSetRecord,
    Workers,
};

/// The database's file name in the store directory.
pub const DATABASE: &str = "title-select.sqlite";
/// The directory, in the store directory, that holds DAT blobs.
pub const DATS: &str = "dats";
/// The directory, in the store directory, that holds the backups taken before migrations.
pub const BACKUPS: &str = "backups";
/// The directory, in the store directory, that holds per-run scratch folders, such as a Selection's Retool run.
/// Each run deletes its own folder.
pub const TMP: &str = "tmp";
/// The file, in the store directory, that an open store holds an exclusive lock on.
pub const LOCK: &str = "lock";

const BUSY_TIMEOUT: Duration = Duration::from_secs(30);
/// Idle reader connections kept for reuse.
const READER_POOL: usize = 4;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("store: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("store: {0}")]
    Io(String),
    /// The request cannot be carried out as asked.
    #[error("{0}")]
    Refused(String),
    /// What the request names does not exist.
    #[error("{0}")]
    NotFound(String),
    /// The request was made against a state that has since changed, such as an edit of an older Profile version.
    #[error("{0}")]
    Conflict(String),
    /// A Source Set already has a Job of this kind Queued or Running.
    #[error("source set {source_set} already has an active {}: job {job}", .kind.as_str())]
    JobActive {
        source_set: String,
        job: i64,
        kind: title_select_api::JobKind,
    },
    #[error(transparent)]
    Audit(#[from] title_select_engine::Error),
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

pub(crate) fn io_error(path: &Path, err: std::io::Error) -> Error {
    Error::Io(format!("{}: {err}", path.display()))
}

pub(crate) fn now() -> String {
    jiff::Timestamp::now().to_string()
}

/// An open store: one writer connection that serialises writes, and a pool of readers. Only one process at a
/// time may open a store, so the writer is the only one.
pub struct Store {
    dir: PathBuf,
    writer: Mutex<Connection>,
    readers: Mutex<Vec<Connection>>,
    /// Holds the exclusive lock on `lock` until the store is dropped.
    _lock: File,
    fault: Mutex<Option<FaultHook>>,
}

/// What a test runs at each [`Fault`] point.
#[doc(hidden)]
pub type FaultHook = std::sync::Arc<dyn Fn(Fault) + Send + Sync>;

/// A point at which a Build stops as if the service were killed there, for tests of its recovery.
#[doc(hidden)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fault {
    /// After the publish intent of the item with this sequence number is committed, before its file is renamed.
    BeforePublish(i64),
    /// After the file of the item with this sequence number is renamed into place, before either folder is synced.
    PublishedUnsynced(i64),
    /// After the file of the item with this sequence number is renamed into place and both folders are synced, before
    /// the store records it.
    AfterPublish(i64),
    /// After the intent to keep aside the file of the item with this sequence number is committed, before it is
    /// moved.
    BeforeKeep(i64),
    /// Before the file the item with this sequence number keeps aside is read whole, to be hashed: not reached when
    /// the hash its Build read while repacking it is trusted.
    KeepRead(i64),
    /// After the file of the item with this sequence number is moved into the retention area, before the store
    /// records it kept.
    AfterKeep(i64),
    /// After the file of the item with this sequence number is moved into the retention area, before either folder is
    /// synced.
    KeptUnsynced(i64),
    /// After the intent to move the entry of a cleanup's item with this sequence number is committed, before it is
    /// renamed.
    BeforeMove(i64),
    /// After the entry of a cleanup's item with this sequence number is renamed, before either folder is synced.
    AfterMove(i64),
    /// After the entry of a cleanup's item with this sequence number is renamed and both folders are synced, before
    /// the store records it moved.
    MoveSynced(i64),
    /// After a folder Title Select is creating in a Managed Folder is made under its temporary name, before it is
    /// opened and checked.
    MadeDir,
    /// After a folder Title Select is creating in a Managed Folder is made and recorded under its temporary name,
    /// before it is renamed into place.
    BeforePublishDir,
    /// As [`Fault::MadeDir`], for a Build's staging directory.
    MadeStaging,
    /// As [`Fault::BeforePublishDir`], for a Build's staging directory.
    BeforePublishStaging,
    /// After the intent to delete the kept file with this id is committed and it is found to be the kept file, before
    /// it is moved into the private folder it is deleted from.
    BeforeDeleteMove(i64),
    /// After the kept file with this id is moved into the private folder it is deleted from, before it is checked.
    AfterDeleteMove(i64),
    /// After what was moved aside to delete the kept file with this id, not being that file, is recorded, before it is
    /// moved back.
    Quarantined(i64),
    /// After what was held aside for the kept file with this id is moved back, before either folder is synced.
    MovedBack(i64),
    /// After what was held aside for the kept file with this id is moved back and both folders are synced, before the
    /// store records it kept again.
    MoveBackSynced(i64),
    /// After the kept file with this id is deleted, before the store records it deleted.
    AfterUnlink(i64),
    /// After a cleanup is planned from what its folder holds, before the plan and the folder's check are stored.
    Scanned,
    /// After the store commits the folder's next generation, before the marker advances to it.
    AfterFinalize,
    /// After the marker advances, before the store records the Build finished.
    AfterMarker,
    /// Before a scan reads an item of an Ingest Folder to hash it: not reached when the hashes it read before are
    /// reused.
    IngestRead,
    /// After an entry of an Ingest Folder is checked to be the file a plan moves, just before it is renamed there.
    IngestRenaming,
    /// After an entry of an Ingest Folder is renamed to where a plan moves it, before either folder is synced.
    IngestMoved,
    /// After the verified copy or repack of an entry of an Ingest Folder is written, before it is renamed into place.
    IngestStaged,
    /// After the copy or repack of an entry of an Ingest Folder is renamed into place, before its original is
    /// disposed of.
    IngestPublished,
    /// After the original of a published copy is moved into the private `.deleting` area, before either folder is
    /// synced.
    IngestIsolated,
    /// After a rename puts back what was moved in place of an Ingest Folder's entry, before either folder is synced.
    IngestRestored,
    /// After an entry is renamed aside into an Ingest Folder's kept files, before either folder is synced.
    IngestSetAside,
    /// After the incremental Audits a DAT import from an Ingest Folder owes are recorded, before the DAT is imported.
    IngestDatOwed,
}

/// Takes the store directory's exclusive lock, failing at once if another process holds it.
fn lock(dir: &Path) -> Result<File> {
    let path = dir.join(LOCK);
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .map_err(|e| io_error(&path, e))?;
    match file.try_lock() {
        Ok(()) => Ok(file),
        Err(TryLockError::WouldBlock) => Err(Error::Refused(format!(
            "{}: the store is in use by another Title Select process",
            dir.display()
        ))),
        Err(TryLockError::Error(e)) => Err(io_error(&path, e)),
    }
}

/// Settings every connection gets (ADR 0003).
fn configure(conn: &Connection) -> Result<()> {
    conn.busy_timeout(BUSY_TIMEOUT)?;
    conn.pragma_update(None, "synchronous", "FULL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    Ok(())
}

impl Store {
    /// Opens the store in `dir`, creating it if needed, migrating it, and collecting blobs that have no row.
    pub fn open(dir: &Path) -> Result<Store> {
        Store::open_with(dir, migrations::MIGRATIONS)
    }

    fn open_with(dir: &Path, migrations: &[&str]) -> Result<Store> {
        let dats = dir.join(DATS);
        std::fs::create_dir_all(&dats).map_err(|e| io_error(&dats, e))?;
        let lock = lock(dir)?;
        let mut writer = Connection::open(dir.join(DATABASE))?;
        configure(&writer)?;
        let mode: String = writer.query_row("PRAGMA journal_mode = WAL", [], |r| r.get(0))?;
        if !mode.eq_ignore_ascii_case("wal") {
            return Err(Error::Io(format!(
                "{}: cannot use WAL mode (got {mode})",
                dir.display()
            )));
        }
        migrations::migrate(&mut writer, dir, migrations)?;
        let store = Store {
            dir: dir.to_path_buf(),
            writer: Mutex::new(writer),
            readers: Mutex::new(Vec::new()),
            _lock: lock,
            fault: Mutex::new(None),
        };
        // Retention is not run here: it runs after an import or an Audit, so a blob restored by one command is
        // still there for the Audit the next command runs.
        store.discard_unfinished_audits()?;
        store.collect_garbage()?;
        Ok(store)
    }

    #[doc(hidden)]
    pub fn inject_fault(&self, fault: Option<Fault>) {
        self.hook_faults(fault.map(|fault| {
            std::sync::Arc::new(move |at: Fault| {
                if at == fault {
                    panic!("injected fault: {fault:?}");
                }
            }) as FaultHook
        }));
    }

    /// Runs `hook` at every [`Fault`] point.
    #[doc(hidden)]
    pub fn hook_faults(&self, hook: Option<FaultHook>) {
        *self.fault.lock().unwrap() = hook;
    }

    /// Runs the hook a test set, which may stop the thread here as a kill would.
    fn fault_point(&self, fault: Fault) {
        let hook = self.fault.lock().unwrap().clone();
        if let Some(hook) = hook {
            hook(fault);
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Where the blob of the DAT Revision with this SHA-256 is kept.
    pub fn blob_path(&self, sha256: &str) -> PathBuf {
        self.dir.join(DATS).join(sha256)
    }

    fn read<T>(&self, f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        let pooled = self.readers.lock().unwrap().pop();
        let conn = match pooled {
            Some(conn) => conn,
            None => {
                let conn = Connection::open_with_flags(
                    self.dir.join(DATABASE),
                    OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
                )?;
                configure(&conn)?;
                conn
            }
        };
        let out = f(&conn);
        let mut readers = self.readers.lock().unwrap();
        if readers.len() < READER_POOL {
            readers.push(conn);
        }
        out
    }

    /// Runs `f` in one write transaction on the writer connection.
    fn write<T>(&self, f: impl FnOnce(&Transaction) -> Result<T>) -> Result<T> {
        self.with_writer(|conn| {
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let out = f(&tx)?;
            tx.commit()?;
            Ok(out)
        })
    }

    /// Runs `f` while holding the writer, so that blob files and the rows that hold them change together.
    fn with_writer<T>(&self, f: impl FnOnce(&mut Connection) -> Result<T>) -> Result<T> {
        let mut conn = self.writer.lock().unwrap();
        f(&mut conn)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pragma(conn: &Connection, name: &str) -> String {
        conn.query_row(&format!("PRAGMA {name}"), [], |r| {
            r.get::<_, rusqlite::types::Value>(0)
        })
        .map(|v| format!("{v:?}"))
        .unwrap()
    }

    #[test]
    fn every_connection_uses_wal_full_sync_foreign_keys_and_a_busy_timeout() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();

        let writer = store.writer.lock().unwrap();
        assert_eq!(pragma(&writer, "journal_mode"), r#"Text("wal")"#);
        let reader = store.read(|c| Ok(pragma(c, "journal_mode"))).unwrap();
        assert_eq!(reader, r#"Text("wal")"#);
        let settings = |c: &Connection| {
            (
                pragma(c, "synchronous"),
                pragma(c, "foreign_keys"),
                pragma(c, "busy_timeout"),
            )
        };
        let expected = (
            "Integer(2)".to_string(),
            "Integer(1)".to_string(),
            "Integer(30000)".to_string(),
        );
        assert_eq!(settings(&writer), expected);
        assert_eq!(store.read(|c| Ok(settings(c))).unwrap(), expected);
    }

    #[test]
    fn a_migration_runs_after_backing_up_the_database() {
        let dir = tempfile::tempdir().unwrap();
        let current = migrations::MIGRATIONS;
        let store = Store::open_with(dir.path(), current).unwrap();
        let blob = store.dir().join("dat.dat");
        std::fs::write(&blob, DAT).unwrap();
        store.import_dat(&blob, "Test", None).unwrap();
        drop(store);
        assert!(!dir.path().join(BACKUPS).exists());

        let second = "CREATE TABLE later (x INTEGER);";
        let store = Store::open_with(dir.path(), &[current, &[second]].concat()).unwrap();

        let backups: Vec<_> = std::fs::read_dir(dir.path().join(BACKUPS))
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        assert_eq!(backups.len(), 1);
        let backup = Connection::open(&backups[0]).unwrap();
        let before = format!("Integer({})", current.len());
        assert_eq!(pragma(&backup, "user_version"), before);
        let revisions: i64 = backup
            .query_row("SELECT count(*) FROM dat_revisions", [], |r| r.get(0))
            .unwrap();
        assert_eq!(revisions, 1);
        let version = store.read(|c| Ok(pragma(c, "user_version"))).unwrap();
        assert_eq!(version, format!("Integer({})", current.len() + 1));
    }

    #[test]
    fn a_second_open_of_the_same_store_fails_until_the_first_is_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let first = Store::open(dir.path()).unwrap();

        let second = Store::open(dir.path());

        let Err(Error::Refused(message)) = second else {
            panic!(
                "expected a refusal, got {second:?}",
                second = second.map(|_| ())
            );
        };
        assert!(message.contains("in use by another Title Select process"));
        drop(first);
        Store::open(dir.path()).unwrap();
    }

    const DAT: &str = "<datafile><header><name>Test</name><version>1</version></header></datafile>";

    #[test]
    fn the_selections_migration_keeps_existing_jobs_as_audits_and_one_active_job_of_each_kind() {
        let dir = tempfile::tempdir().unwrap();
        let before = &migrations::MIGRATIONS[..2];
        let store = Store::open_with(dir.path(), before).unwrap();
        // The rows a store at schema 2 holds, written as that schema's code wrote them.
        store
            .write(|tx| {
                tx.execute_batch(
                    "INSERT INTO dats (provider, name) VALUES ('Test', 'Test');
                     INSERT INTO dat_revisions (dat_id, sha256, version, file_name, imported_at)
                         SELECT id, 'abc', '1', 'Test.dat', 'then' FROM dats;
                     INSERT INTO source_sets (name, root, path, dat_id, added_at)
                         SELECT 'Set', 'sets', 'Set', id, 'then' FROM dats;
                     INSERT INTO jobs (state, source_set_id, dat_revision_id, actor, readers, hashers, created_at)
                         SELECT 'queued', s.id, r.id, 'Operator', 1, 1, 'then' FROM source_sets s, dat_revisions r;",
                )?;
                Ok(())
            })
            .unwrap();
        drop(store);

        let store = Store::open(dir.path()).unwrap();

        let jobs = store.jobs(10).unwrap();
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].kind, title_select_api::JobKind::Audit);
        assert_eq!((jobs[0].profile.clone(), jobs[0].selection), (None, None));
        let insert = |kind: &str, profile: &str| {
            store.write(|tx| {
                tx.execute_batch(&format!(
                    "INSERT INTO jobs (state, kind, profile_version_id, source_set_id, dat_revision_id, actor,
                                       readers, hashers, created_at)
                         SELECT 'queued', '{kind}', {profile}, source_set_id, dat_revision_id, 'Operator', 1, 1,
                                'now' FROM jobs LIMIT 1;"
                ))?;
                Ok(())
            })
        };
        assert!(insert("audit", "NULL").is_err(), "a second active audit");
        assert!(
            insert("selection", "NULL").is_err(),
            "a selection with no profile version"
        );
        store
            .save_profile(
                "Mine",
                &Default::default(),
                None,
                "Operator",
                &[],
                Workers {
                    readers: 1,
                    hashers: 1,
                },
            )
            .unwrap();
        insert("selection", "(SELECT id FROM profile_versions)").unwrap();
        assert!(insert("selection", "(SELECT id FROM profile_versions)").is_err());
    }

    #[test]
    fn the_builds_migration_rebuilds_jobs_keeping_their_references_and_gives_the_store_an_id() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_with(dir.path(), &migrations::MIGRATIONS[..3]).unwrap();
        // A Selection Job and the Selection that refers back to it, as schema 3's code wrote them.
        store
            .write(|tx| {
                tx.execute_batch(
                    "INSERT INTO dats (provider, name) VALUES ('Test', 'Test');
                     INSERT INTO dat_revisions (dat_id, sha256, version, file_name, imported_at)
                         SELECT id, 'abc', '1', 'Test.dat', 'then' FROM dats;
                     INSERT INTO source_sets (name, root, path, dat_id, added_at)
                         SELECT 'Set', 'sets', 'Set', id, 'then' FROM dats;
                     INSERT INTO profiles (name, created_at) VALUES ('Mine', 'then');
                     INSERT INTO profile_versions (profile_id, version, json, sha256, saved_at, saved_by)
                         SELECT id, 1, '{}', 'def', 'then', 'Operator' FROM profiles;
                     INSERT INTO jobs (state, kind, profile_version_id, source_set_id, dat_revision_id, actor,
                                       readers, hashers, created_at)
                         SELECT 'succeeded', 'selection', v.id, s.id, r.id, 'Operator', 1, 1, 'then'
                         FROM profile_versions v, source_sets s, dat_revisions r;
                     INSERT INTO selections (source_set_id, dat_revision_id, profile_version_id, job_id, system,
                                             system_settings, pins, summary, coverage_drift, stale_choices,
                                             created_at)
                         SELECT source_set_id, dat_revision_id, profile_version_id, id, 'Test', 0, '{}', '{}',
                                '{}', '{}', 'then' FROM jobs;
                     UPDATE jobs SET selection_id = (SELECT id FROM selections);",
                )?;
                Ok(())
            })
            .unwrap();
        drop(store);

        let store = Store::open(dir.path()).unwrap();

        let jobs = store.jobs(10).unwrap();
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].kind, title_select_api::JobKind::Selection);
        assert_eq!(jobs[0].selection, Some(1));
        assert_eq!((jobs[0].managed_folder, jobs[0].plan), (None, None));
        let id = store.read(folders::store_id).unwrap();
        assert_eq!(id.len(), 32);
        let build_without_a_folder = store.write(|tx| {
            tx.execute_batch(
                "INSERT INTO jobs (state, kind, source_set_id, dat_revision_id, actor, readers, hashers,
                                   created_at)
                     SELECT 'queued', 'build', source_set_id, dat_revision_id, 'Operator', 1, 1, 'now'
                     FROM jobs LIMIT 1;",
            )?;
            Ok(())
        });
        assert!(build_without_a_folder.is_err());
        let foreign_keys = store.read(|c| Ok(pragma(c, "foreign_keys"))).unwrap();
        assert_eq!(foreign_keys, "Integer(1)");
    }

    #[test]
    fn the_ingest_migration_rebuilds_jobs_keeping_every_column_of_every_job() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_with(dir.path(), &migrations::MIGRATIONS[..7]).unwrap();
        store
            .write(|tx| {
                tx.execute_batch(
                    "INSERT INTO dats (provider, name) VALUES ('Test', 'Test');
                     INSERT INTO dat_revisions (dat_id, sha256, version, file_name, imported_at)
                         SELECT id, 'abc', '1', 'Test.dat', 'then' FROM dats;
                     INSERT INTO source_sets (name, root, path, dat_id, added_at)
                         SELECT 'Set', 'sets', 'Set', id, 'then' FROM dats;
                     INSERT INTO jobs (state, source_set_id, dat_revision_id, actor, readers, hashers, created_at,
                                       finished_at, error)
                         SELECT 'failed', s.id, r.id, 'Operator', 2, 3, 'then', 'later', 'it stopped'
                         FROM source_sets s, dat_revisions r;
                     INSERT INTO jobs (state, source_set_id, dat_revision_id, actor, readers, hashers, created_at)
                         SELECT 'succeeded', s.id, r.id, 'Operator', 1, 1, 'then'
                         FROM source_sets s, dat_revisions r;",
                )?;
                Ok(())
            })
            .unwrap();
        drop(store);
        let store = Store::open_with(dir.path(), &migrations::MIGRATIONS[..8]).unwrap();
        store
            .write(|tx| {
                tx.execute("UPDATE jobs SET evidence = 'incremental' WHERE id = 2", [])?;
                Ok(())
            })
            .unwrap();
        let rows =
            |c: &Connection, columns: &[String]| -> Result<Vec<Vec<rusqlite::types::Value>>> {
                let mut stmt = c.prepare(&format!(
                    "SELECT {} FROM jobs ORDER BY id",
                    columns.join(", ")
                ))?;
                Ok(stmt
                    .query_map([], |r| (0..columns.len()).map(|i| r.get(i)).collect())?
                    .collect::<rusqlite::Result<Vec<_>>>()?)
            };
        let columns: Vec<String> = store
            .read(|c| {
                let mut stmt = c.prepare("SELECT name FROM pragma_table_info('jobs')")?;
                Ok(stmt
                    .query_map([], |r| r.get(0))?
                    .collect::<rusqlite::Result<Vec<_>>>()?)
            })
            .unwrap();
        assert!(columns.contains(&"evidence".to_owned()));
        let before = store.read(|c| rows(c, &columns)).unwrap();
        drop(store);

        let store = Store::open(dir.path()).unwrap();

        assert_eq!(store.read(|c| rows(c, &columns)).unwrap(), before);
        let jobs = store.jobs(10).unwrap();
        assert_eq!(
            jobs.iter().map(|j| j.evidence).collect::<Vec<_>>(),
            [
                Some(title_select_api::AuditEvidence::Incremental),
                Some(title_select_api::AuditEvidence::Full)
            ]
        );
        let foreign_keys = store.read(|c| Ok(pragma(c, "foreign_keys"))).unwrap();
        assert_eq!(foreign_keys, "Integer(1)");
    }

    #[test]
    fn a_store_from_a_newer_schema_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let first = migrations::MIGRATIONS[0];
        drop(Store::open_with(dir.path(), &[first, "SELECT 1;"]).unwrap());

        assert!(matches!(
            Store::open_with(dir.path(), &[first]),
            Err(Error::Refused(_))
        ));
    }
}
