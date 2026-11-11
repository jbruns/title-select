//! `tools/clone-lists/fetch` unpacks only regular files and folders from the pinned archive, so an archive that
//! links or names something outside its own folder can't put another file into the clone lists.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use sha2::{Digest, Sha256};

const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";

enum Kind<'a> {
    File(&'a str),
    Dir,
    Symlink(&'a str),
    Hardlink(&'a str),
    Fifo,
    CharDevice,
}

/// A ustar member: its name as stored, and what it is.
fn header(name: &str, kind: &Kind) -> Vec<u8> {
    let mut h = vec![0u8; 512];
    let put =
        |h: &mut Vec<u8>, at: usize, text: &[u8]| h[at..at + text.len()].copy_from_slice(text);
    assert!(name.len() < 100, "{name}");
    put(&mut h, 0, name.as_bytes());
    let (mode, size, flag, link) = match kind {
        Kind::File(text) => ("0000644", text.len(), b'0', ""),
        Kind::Dir => ("0000755", 0, b'5', ""),
        Kind::Symlink(to) => ("0000777", 0, b'2', *to),
        Kind::Hardlink(to) => ("0000644", 0, b'1', *to),
        Kind::Fifo => ("0000644", 0, b'6', ""),
        Kind::CharDevice => ("0000644", 0, b'3', ""),
    };
    put(&mut h, 100, mode.as_bytes());
    put(&mut h, 108, b"0000000");
    put(&mut h, 116, b"0000000");
    put(&mut h, 124, format!("{size:011o}").as_bytes());
    put(&mut h, 136, b"00000000000");
    h[156] = flag;
    put(&mut h, 157, link.as_bytes());
    put(&mut h, 257, b"ustar\x0000");
    put(&mut h, 329, b"0000000");
    put(&mut h, 337, b"0000000");
    put(&mut h, 148, b"        ");
    let sum: u32 = h.iter().map(|&b| u32::from(b)).sum();
    put(&mut h, 148, format!("{sum:06o}\0 ").as_bytes());
    h
}

fn archive(members: &[(String, Kind)]) -> Vec<u8> {
    let mut tar = Vec::new();
    for (name, kind) in members {
        tar.extend(header(name, kind));
        if let Kind::File(text) = kind {
            tar.extend(text.as_bytes());
            tar.resize(tar.len().div_ceil(512) * 512, 0);
        }
    }
    tar.resize(tar.len() + 1024, 0);
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    gz.write_all(&tar).unwrap();
    gz.finish().unwrap()
}

/// Runs a copy of the script pinned to an archive of `members`, served from a local folder, and returns its
/// output and the folder it was asked to fill.
fn fetch(work: &Path, members: &[(String, Kind)]) -> (std::process::Output, PathBuf) {
    let bytes = archive(members);
    let repo = work.join("repo");
    std::fs::create_dir_all(repo.join("tools/clone-lists")).unwrap();
    std::fs::create_dir_all(repo.join("third_party/clone-lists")).unwrap();
    std::fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tools/clone-lists/fetch"),
        repo.join("tools/clone-lists/fetch"),
    )
    .unwrap();
    let upstream = work.join("upstream");
    std::fs::create_dir_all(upstream.join("archive")).unwrap();
    std::fs::write(upstream.join(format!("archive/{COMMIT}.tar.gz")), &bytes).unwrap();
    std::fs::write(
        repo.join("third_party/clone-lists/pins.toml"),
        format!(
            "[clone_lists]\nrepository = \"file://{}\"\nbranch = \"main\"\ncommit = \"{COMMIT}\"\n\
             archive_sha256 = \"{}\"\n",
            upstream.display(),
            Sha256::digest(&bytes)
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>()
        ),
    )
    .unwrap();
    let dest = work.join("dest");
    let output = Command::new("sh")
        .arg(repo.join("tools/clone-lists/fetch"))
        .arg(&dest)
        .output()
        .unwrap();
    (output, dest)
}

fn top(path: &str) -> String {
    format!("retool-clonelists-metadata-{COMMIT}/{path}")
}

#[test]
fn the_clone_lists_unpack_from_an_archive_of_regular_files_and_folders() {
    let work = tempfile::tempdir().unwrap();
    let (output, dest) = fetch(
        work.path(),
        &[
            (top(""), Kind::Dir),
            (top("LICENSE"), Kind::File("BSD")),
            (top("README.md"), Kind::File("not needed")),
            (top("config/"), Kind::Dir),
            (top("config/internal-config.json"), Kind::File("{}")),
            (top("clonelists/"), Kind::Dir),
            (top("clonelists/A.json"), Kind::File("[]")),
            (top("metadata/"), Kind::Dir),
        ],
    );
    assert!(output.status.success(), "{output:?}");
    let read = |p: &str| std::fs::read_to_string(dest.join(p)).unwrap();
    assert_eq!(read("LICENSE"), "BSD");
    assert_eq!(read("config/internal-config.json"), "{}");
    assert_eq!(read("clonelists/A.json"), "[]");
    assert_eq!(read("commit"), format!("{COMMIT}\n"));
    assert!(dest.join("metadata").is_dir() && dest.join("mias").is_dir());
    assert!(!dest.join("README.md").exists());
}

#[test]
fn an_archive_with_links_special_files_or_paths_outside_it_is_refused() {
    let outside = tempfile::tempdir().unwrap();
    let secret = outside.path().join("secret");
    std::fs::write(&secret, "not a licence").unwrap();
    let secret = secret.to_str().unwrap().to_owned();
    let escape = format!("{}/escaped", outside.path().display());
    let cases: Vec<(&str, Vec<(String, Kind)>)> = vec![
        (
            "a symlinked licence",
            vec![(top("LICENSE"), Kind::Symlink(&secret))],
        ),
        (
            "a symlinked config",
            vec![
                (top("config/"), Kind::Dir),
                (top("config/internal-config.json"), Kind::Symlink(&secret)),
            ],
        ),
        (
            "a symlinked folder",
            vec![(
                top("clonelists"),
                Kind::Symlink(outside.path().to_str().unwrap()),
            )],
        ),
        (
            "a hardlink",
            vec![(top("LICENSE"), Kind::Hardlink(&secret))],
        ),
        ("a fifo", vec![(top("clonelists/fifo"), Kind::Fifo)]),
        ("a device", vec![(top("clonelists/tty"), Kind::CharDevice)]),
        ("an absolute path", vec![(escape.clone(), Kind::File("x"))]),
        (
            "a .. component",
            vec![(top("clonelists/../../../escaped"), Kind::File("x"))],
        ),
    ];
    for (what, extra) in cases {
        let work = tempfile::tempdir().unwrap();
        // Each case replaces what it names of a sound archive.
        let mut members: Vec<(String, Kind)> = [
            (top("LICENSE"), Kind::File("BSD")),
            (top("config/"), Kind::Dir),
            (top("config/internal-config.json"), Kind::File("{}")),
        ]
        .into_iter()
        .filter(|(name, _)| !extra.iter().any(|(n, _)| n == name))
        .collect();
        members.extend(extra);
        let (output, dest) = fetch(work.path(), &members);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(!output.status.success(), "{what}: {output:?}");
        assert!(
            stderr.contains("fetch: the clone-list archive"),
            "{what}: {stderr}"
        );
        assert!(!dest.exists(), "{what}");
        assert!(!Path::new(&escape).exists(), "{what}");
    }
}
