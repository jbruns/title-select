//! MAME support files you drop in (ADR 0012's amendment): kept unmodified by SHA-256, with the MAME version their
//! header names.

use title_select_api::ImportOutcome;
use title_select_api::support::{SupportFileKind, SupportFileOrigin};
use title_select_store::{Error, Store};

fn ini(file: &str, version: &str) -> Vec<u8> {
    format!(
        "[FOLDER_SETTINGS]\r\nRootFolderIcon mame\r\n\r\n;; {file} {version} / 09-Sep-25 / MAME {version} ;;\r\n\r\n\
         [ROOT_FOLDER]\r\n\r\n[0 to 10 (Worst)]\r\npong\r\n"
    )
    .into_bytes()
}

#[test]
fn a_support_file_you_drop_in_is_kept_by_its_sha256_with_the_mame_version_its_header_names() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    for (kind, version) in [
        (SupportFileKind::Catver, "0.289"),
        (SupportFileKind::Languages, "0.289"),
        (SupportFileKind::Bestgames, "0.280"),
    ] {
        let bytes = ini(kind.file_name(), version);
        // Its name doesn't decide what it is: its header does.
        let (file, outcome) = store
            .import_support_file(&bytes, "download.ini", None)
            .unwrap();
        assert_eq!(outcome, ImportOutcome::Added);
        assert_eq!(file.kind, kind);
        assert_eq!(file.mame_version, version);
        assert_eq!(file.file, "download.ini");
        assert_eq!(file.origin, SupportFileOrigin::Import);
        assert_eq!(file.commit, None);
        let kept = dir.path().join("support_files").join(&file.sha256);
        assert_eq!(std::fs::read(&kept).unwrap(), bytes, "kept unmodified");

        let (again, outcome) = store
            .import_support_file(&bytes, "again.ini", None)
            .unwrap();
        assert_eq!(outcome, ImportOutcome::AlreadyImported);
        assert_eq!(again, file);
    }
    assert_eq!(store.support_files().unwrap().len(), 3);
}

#[test]
fn a_dropped_in_support_file_whose_file_was_lost_is_restored() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let bytes = ini("catver.ini", "0.289");
    let (file, _) = store
        .import_support_file(&bytes, "catver.ini", None)
        .unwrap();
    std::fs::remove_file(dir.path().join("support_files").join(&file.sha256)).unwrap();
    let (_, outcome) = store
        .import_support_file(&bytes, "catver.ini", None)
        .unwrap();
    assert_eq!(outcome, ImportOutcome::Restored);
    assert!(
        dir.path()
            .join("support_files")
            .join(&file.sha256)
            .is_file()
    );
}

#[test]
fn a_file_that_is_not_a_support_file_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    for (bytes, why) in [
        (b"[Category]\r\npong=Sports\r\n".to_vec(), "header"),
        (ini("series.ini", "0.289"), "series.ini"),
        (ini("catver.ini", "next"), "MAME version"),
        (vec![0xff, 0xfe, 0x00], "header"),
    ] {
        match store.import_support_file(&bytes, "x.ini", None) {
            Err(Error::Refused(e)) => assert!(e.contains(why), "{why}: {e}"),
            other => panic!("{why}: {other:?}"),
        }
    }
    assert!(store.support_files().unwrap().is_empty());
}

#[test]
fn garbage_collection_keeps_support_files_and_removes_strays() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let (file, _) = store
        .import_support_file(&ini("catver.ini", "0.289"), "catver.ini", None)
        .unwrap();
    let support = dir.path().join("support_files");
    std::fs::write(support.join(".stray.tmp"), b"x").unwrap();
    let removed = store.collect_garbage().unwrap();
    assert_eq!(removed, ["support_files/.stray.tmp"]);
    assert!(support.join(&file.sha256).is_file());
}

#[test]
fn a_selection_reads_the_newest_mame_versions_support_file_of_each_kind() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let import = |file: &str, version: &str| {
        store
            .import_support_file(&ini(file, version), file, None)
            .unwrap()
            .0
            .sha256
    };
    import("catver.ini", "0.288");
    let catver = import("catver.ini", "0.289");
    // An older file dropped in later doesn't replace the newer one.
    import("catver.ini", "0.99");
    let languages = import("languages.ini", "0.289");

    let kept = dir.path().join("support_files");
    let mut expected = vec![kept.join(catver), kept.join(languages)];
    expected.sort();
    let mut read = store.selection_support_files().unwrap();
    read.sort();
    assert_eq!(read, expected);
}
