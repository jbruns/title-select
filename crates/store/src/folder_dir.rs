//! Directory handles for Managed Folders. A folder is opened once, from its Root's configured path through each
//! component of its relative path without following a symlink, and every read or change in it is then made relative
//! to that handle with the `*at` calls: a folder or staging directory swapped for a symlink after it was checked
//! cannot redirect a write. Files are created exclusively and opened without following a symlink, and a file is
//! renamed into place only by a rename that refuses to replace anything.

use std::ffi::OsStr;
use std::fs::File;
use std::io::{self, Read};
use std::os::fd::{AsFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use rustix::fs::{AtFlags, Dir, FileType, Mode, OFlags, RenameFlags, Stat};
use title_select_api::build::FileStat;

/// An open directory and the path it was opened at, for messages.
pub(super) struct FolderDir {
    fd: OwnedFd,
    path: PathBuf,
}

const DIR_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::DIRECTORY)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);

fn file_mode() -> Mode {
    // The umask applies.
    Mode::RUSR | Mode::WUSR | Mode::RGRP | Mode::WGRP | Mode::ROTH | Mode::WOTH
}

fn dir_mode() -> Mode {
    Mode::RWXU | Mode::RWXG | Mode::RWXO
}

/// A single path component: not empty, `.` or `..`, and holding no `/`.
fn component(name: &str) -> io::Result<&str> {
    if name.is_empty() || name == "." || name == ".." || name.contains('/') || name.contains('\0') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{name:?} is not a single file name"),
        ));
    }
    Ok(name)
}

#[cfg(test)]
thread_local! {
    /// Each directory this thread synced, in order.
    pub(crate) static SYNCED: std::cell::RefCell<Vec<PathBuf>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// What a file or directory is, as opposed to the name it is found at: its device and inode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Identity {
    pub dev: u64,
    pub ino: u64,
}

impl Identity {
    // The field types differ by platform.
    #[allow(clippy::unnecessary_cast)]
    pub fn of(st: &Stat) -> Identity {
        Identity {
            dev: st.st_dev as u64,
            ino: st.st_ino as u64,
        }
    }

    /// The identity of the open `fd`, from `fstat`.
    pub fn of_fd(fd: impl AsFd) -> io::Result<Identity> {
        Ok(Identity::of(&rustix::fs::fstat(fd)?))
    }

    /// As stored in SQLite, which has signed integers only.
    pub fn to_sql(self) -> (i64, i64) {
        (self.dev as i64, self.ino as i64)
    }

    pub fn from_sql(dev: i64, ino: i64) -> Identity {
        Identity {
            dev: dev as u64,
            ino: ino as u64,
        }
    }
}

impl From<Identity> for title_select_api::build::FileIdentity {
    fn from(id: Identity) -> Self {
        title_select_api::build::FileIdentity {
            dev: id.dev,
            ino: id.ino,
        }
    }
}

impl From<title_select_api::build::FileIdentity> for Identity {
    fn from(id: title_select_api::build::FileIdentity) -> Self {
        Identity {
            dev: id.dev,
            ino: id.ino,
        }
    }
}

/// A file's status change time in nanoseconds since the Unix epoch: it moves on any change to the file's content or
/// metadata, and cannot be set back.
#[allow(clippy::unnecessary_cast)]
pub(super) fn ctime_ns(st: &Stat) -> i128 {
    st.st_ctime as i128 * 1_000_000_000 + st.st_ctime_nsec as i128
}

/// A file's size and modification time, from `lstat`.
pub(super) fn file_stat(st: &Stat) -> FileStat {
    FileStat {
        size: st.st_size as u64,
        mtime_ns: st.st_mtime as i128 * 1_000_000_000 + st.st_mtime_nsec as i128,
    }
}

impl FolderDir {
    /// Opens the folder `rel` (`/`-separated; empty for the Root itself) in the Root at `root`. The Root's own path is followed as configured;
    /// no component of `rel` may be a symlink.
    pub fn open(root: &Path, rel: &str) -> io::Result<FolderDir> {
        let fd = rustix::fs::open(
            root,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|e| with_path(root, e.into()))?;
        let mut dir = FolderDir {
            fd,
            path: root.to_path_buf(),
        };
        if !rel.is_empty() {
            for part in rel.split('/') {
                dir = dir.sub(part)?;
            }
        }
        Ok(dir)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn at(&self, name: &str) -> PathBuf {
        self.path.join(name)
    }

    /// The directory `name` in this one, not followed if it is a symlink.
    pub fn sub(&self, name: &str) -> io::Result<FolderDir> {
        let name = component(name)?;
        let fd = rustix::fs::openat(&self.fd, name, DIR_FLAGS, Mode::empty())
            .map_err(|e| self.error(name, e))?;
        Ok(FolderDir {
            fd,
            path: self.at(name),
        })
    }

    /// Creates the directory `name`, failing if anything is there.
    pub fn mkdir(&self, name: &str) -> io::Result<()> {
        let name = component(name)?;
        rustix::fs::mkdirat(&self.fd, name, dir_mode()).map_err(|e| self.error(name, e))
    }

    /// Creates the directory `name` for this service's user only, failing if anything is there.
    pub fn mkdir_private(&self, name: &str) -> io::Result<()> {
        let name = component(name)?;
        rustix::fs::mkdirat(&self.fd, name, Mode::RWXU).map_err(|e| self.error(name, e))
    }

    /// Checks, through the open descriptor, that this is a directory [`FolderDir::mkdir_private`] could have made:
    /// owned by this process's effective user, with permission bits exactly `rwx------`, and, with `empty`, holding
    /// nothing. Returns its identity, or why it is not.
    ///
    /// The Managed Folder lets only the service's own user rename entries in it, and a process running as that user
    /// can rewrite the store as well, so it is not defended against. What this rejects is a directory another user, or
    /// an earlier leftover, put at a name Title Select is about to trust.
    pub fn private(&self, empty: bool) -> io::Result<std::result::Result<Identity, String>> {
        let st = rustix::fs::fstat(&self.fd).map_err(|e| with_path(&self.path, e.into()))?;
        let mode = st.st_mode as u32 & 0o777;
        if FolderDir::kind(&st) != FileType::Directory {
            return Ok(Err("it is not a directory".into()));
        }
        if st.st_uid != rustix::process::geteuid().as_raw() {
            return Ok(Err("it is owned by another user".into()));
        }
        if mode != 0o700 {
            return Ok(Err(format!(
                "its permissions are {mode:o}, not the 700 Title Select creates it with"
            )));
        }
        if empty && !self.entries()?.is_empty() {
            return Ok(Err("it is not empty".into()));
        }
        Ok(Ok(Identity::of(&st)))
    }

    /// The identity of this directory as it was opened.
    pub fn identity(&self) -> io::Result<Identity> {
        Identity::of_fd(&self.fd).map_err(|e| with_path(&self.path, e))
    }

    /// Creates the new file `name` for reading and writing, failing if anything, a symlink included, is there.
    pub fn create_new(&self, name: &str) -> io::Result<File> {
        let name = component(name)?;
        let flags =
            OFlags::RDWR | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC;
        let fd = rustix::fs::openat(&self.fd, name, flags, file_mode())
            .map_err(|e| self.error(name, e))?;
        Ok(File::from(fd))
    }

    /// Opens the regular file `name` for reading and writing, refusing a symlink or anything else.
    pub fn open_write(&self, name: &str) -> io::Result<File> {
        let name = component(name)?;
        let flags = OFlags::RDWR | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC;
        let fd = rustix::fs::openat(&self.fd, name, flags, Mode::empty())
            .map_err(|e| self.error(name, e))?;
        if FileType::from_raw_mode(rustix::fs::fstat(&fd)?.st_mode) != FileType::RegularFile {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{} is not a regular file", self.at(name).display()),
            ));
        }
        Ok(File::from(fd))
    }

    /// Opens the regular file `name` for reading, refusing a symlink or anything else.
    pub fn open_file(&self, name: &str) -> io::Result<File> {
        let name = component(name)?;
        let flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC;
        let fd = rustix::fs::openat(&self.fd, name, flags, Mode::empty())
            .map_err(|e| self.error(name, e))?;
        if FileType::from_raw_mode(rustix::fs::fstat(&fd)?.st_mode) != FileType::RegularFile {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{} is not a regular file", self.at(name).display()),
            ));
        }
        Ok(File::from(fd))
    }

    /// Holds `name` open while it is the regular file `id`, so that while a rename moves it and the result is
    /// checked, its inode number can't be freed and reused by a file put in its place; `None` when it isn't.
    pub fn pin(&self, name: &str, id: Identity) -> Option<File> {
        let file = self.open_file(name).ok()?;
        (Identity::of_fd(&file).ok()? == id).then_some(file)
    }

    /// The `lstat` of `name`, or `None` if nothing is there.
    pub fn stat(&self, name: &str) -> io::Result<Option<Stat>> {
        let name = component(name)?;
        match rustix::fs::statat(&self.fd, name, AtFlags::SYMLINK_NOFOLLOW) {
            Ok(st) => Ok(Some(st)),
            Err(rustix::io::Errno::NOENT) => Ok(None),
            Err(e) => Err(self.error(name, e)),
        }
    }

    pub fn kind(st: &Stat) -> FileType {
        FileType::from_raw_mode(st.st_mode)
    }

    /// Renames `from` in this directory to `to` in `dest`, failing with `AlreadyExists` rather than replacing
    /// anything at `to`. There is no fallback: where the filesystem cannot rename exclusively, it fails.
    pub fn rename_new(&self, from: &str, dest: &FolderDir, to: &str) -> io::Result<()> {
        let (from, to) = (component(from)?, component(to)?);
        match rustix::fs::renameat_with(&self.fd, from, &dest.fd, to, RenameFlags::NOREPLACE) {
            Ok(()) => Ok(()),
            Err(rustix::io::Errno::EXIST) => Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("{} already exists", dest.at(to).display()),
            )),
            Err(
                e @ (rustix::io::Errno::INVAL
                | rustix::io::Errno::NOSYS
                | rustix::io::Errno::NOTSUP),
            ) => Err(io::Error::new(
                io::ErrorKind::Unsupported,
                format!(
                    "{}: the filesystem cannot rename a file without replacing what is there ({e}), so nothing \
                     is built into it",
                    dest.path.display()
                ),
            )),
            Err(e) => Err(dest.error(to, e)),
        }
    }

    /// Renames `from` over `to`, both in this directory, replacing `to` itself if it is there.
    pub fn replace(&self, from: &str, to: &str) -> io::Result<()> {
        let (from, to) = (component(from)?, component(to)?);
        rustix::fs::renameat(&self.fd, from, &self.fd, to).map_err(|e| self.error(to, e))
    }

    /// Removes the file or symlink `name`; a symlink's target is never touched.
    pub fn unlink(&self, name: &str) -> io::Result<()> {
        let name = component(name)?;
        rustix::fs::unlinkat(&self.fd, name, AtFlags::empty()).map_err(|e| self.error(name, e))
    }

    /// Removes the empty directory `name`.
    pub fn rmdir(&self, name: &str) -> io::Result<()> {
        let name = component(name)?;
        rustix::fs::unlinkat(&self.fd, name, AtFlags::REMOVEDIR).map_err(|e| self.error(name, e))
    }

    /// Makes the directory's entries durable.
    pub fn sync(&self) -> io::Result<()> {
        #[cfg(test)]
        SYNCED.with(|synced| synced.borrow_mut().push(self.path.clone()));
        rustix::fs::fsync(&self.fd).map_err(|e| with_path(&self.path, e.into()))
    }

    /// The names in the directory, other than `.` and `..`, with their types as the listing gives them.
    pub fn entries(&self) -> io::Result<Vec<(String, FileType)>> {
        let mut dir = Dir::read_from(&self.fd).map_err(|e| with_path(&self.path, e.into()))?;
        let mut names = Vec::new();
        while let Some(entry) = dir.read() {
            let entry = entry.map_err(|e| with_path(&self.path, e.into()))?;
            let bytes = entry.file_name().to_bytes();
            if bytes == b"." || bytes == b".." {
                continue;
            }
            names.push((
                OsStr::from_bytes(bytes).to_string_lossy().into_owned(),
                entry.file_type(),
            ));
        }
        Ok(names)
    }

    /// Reads the regular file `name`, up to `limit` bytes.
    pub fn read(&self, name: &str, limit: u64) -> io::Result<Vec<u8>> {
        let mut bytes = Vec::new();
        self.open_file(name)?
            .take(limit + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| with_path(&self.at(name), e))?;
        if bytes.len() as u64 > limit {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{} is larger than {limit} bytes", self.at(name).display()),
            ));
        }
        Ok(bytes)
    }

    fn error(&self, name: &str, e: rustix::io::Errno) -> io::Error {
        with_path(&self.at(name), e.into())
    }
}

impl AsFd for FolderDir {
    fn as_fd(&self) -> std::os::fd::BorrowedFd<'_> {
        self.fd.as_fd()
    }
}

fn with_path(path: &Path, e: io::Error) -> io::Error {
    io::Error::new(e.kind(), format!("{}: {e}", path.display()))
}
