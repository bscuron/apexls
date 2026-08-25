//! Regression coverage for a real crash: `Path::extension()` splits on
//! the *last* `.` in a file name regardless of a leading one, so an
//! Emacs-style lock file (`.#Foo.cls`, ~30 bytes of plain text like
//! `user@host.pid:boot-time`, written alongside a file while it's being
//! edited) has extension `cls` and used to be discovered and fed into the
//! binder as a genuine new Apex class. Because a *new* file's appearance
//! forces `apex-binder`'s incremental rebind down its conservative
//! "declarations changed somewhere -- rebind everything" fallback, this
//! was observed (via a real user report, driving `apexls-server`'s
//! filesystem watcher against a real editing session) to panic inside
//! `SymbolTable::get` with an index-out-of-bounds error shortly after a
//! rename, which then poisoned `bind.cache`'s `Mutex` and broke every
//! later rebuild for the rest of the session.

use std::path::Path;

#[test]
fn a_leading_dot_lock_file_is_not_relevant() {
    assert!(!apex_discover::is_relevant_path(Path::new(
        ".#Widget.cls"
    )));
    assert!(!apex_discover::is_relevant_path(Path::new(
        ".#Widget.trigger"
    )));
    assert!(!apex_discover::is_relevant_path(Path::new(
        ".#Widget.object-meta.xml"
    )));
    assert!(!apex_discover::is_relevant_path(Path::new(
        ".#Widget.field-meta.xml"
    )));
}

#[test]
fn an_ordinary_file_is_still_relevant() {
    assert!(apex_discover::is_relevant_path(Path::new("Widget.cls")));
    assert!(apex_discover::is_relevant_path(Path::new(
        "Widget.trigger"
    )));
}

#[test]
fn an_autosave_file_was_already_excluded_by_the_extension_check() {
    // `#Widget.cls#`'s extension is `cls#` (the trailing `#` is part of
    // it), not `cls` -- this was never a bug, but worth pinning down
    // explicitly alongside the lock-file case above.
    assert!(!apex_discover::is_relevant_path(Path::new("#Widget.cls#")));
}

#[test]
fn a_directory_walk_skips_a_real_lock_file_sitting_next_to_a_real_class() {
    let dir = std::env::temp_dir().join(format!(
        "apex-discover-hidden-artifact-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("Widget.cls"), "public class Widget {}").unwrap();
    std::fs::write(dir.join(".#Widget.cls"), "bscur@host.12345:987654321").unwrap();

    let found = apex_discover::find_apex_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    assert_eq!(
        found,
        vec![dir.join("Widget.cls")],
        "the lock file must never be discovered as a project file"
    );
}
