//! Binder-level tests for `apex_binder::complete_at` (`BACKLOG.md`'s
//! `textDocument/completion` entry): given a byte offset, what candidates
//! does the resolved context produce. No LSP protocol involved -- see
//! `crates/apexls-server/tests/completion.rs` for the end-to-end
//! protocol-level counterpart. Follows the same fixture-on-disk pattern
//! as `position_lookup.rs`.

use apex_binder::{complete_at, BoundProgram, CompletionCandidateKind, SymbolKind};

fn write_fixture_dir(name: &str, files: &[(&str, &str)]) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("apex-binder-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for (file_name, src) in files {
        std::fs::write(dir.join(file_name), src).unwrap();
    }
    dir
}

fn file_for_class(program: &BoundProgram, class_name: &str) -> apex_binder::FileId {
    program
        .symbols
        .iter()
        .find(|(_, s)| s.kind == SymbolKind::Class && s.name == class_name)
        .map(|(_, s)| s.file)
        .unwrap_or_else(|| panic!("{class_name} should have been collected"))
}

/// Splits `src` on a `|` cursor marker, returning the marker-stripped
/// source and the byte offset it marked -- far less error-prone than
/// hand-computing offsets for every fixture.
fn split_cursor(src: &str) -> (String, u32) {
    let idx = src.find('|').expect("fixture must contain a `|` cursor marker");
    let mut out = String::with_capacity(src.len() - 1);
    out.push_str(&src[..idx]);
    out.push_str(&src[idx + 1..]);
    (out, idx as u32)
}

fn labels_of(candidates: &[apex_binder::CompletionCandidate]) -> Vec<&str> {
    candidates.iter().map(|c| c.label.as_str()).collect()
}

#[test]
fn locals_and_params_are_all_visible_across_nested_scopes() {
    let (src, offset) = split_cursor(
        "public class Foo { \
             public void run(Integer x) { \
                 Integer y = 0; \
                 if (true) { Integer z = 0; |} \
             } \
         }",
    );
    let dir = write_fixture_dir("completion-locals-nested", &[("Foo.cls", &src)]);
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    let file = file_for_class(&program, "Foo");
    let ctx = complete_at(&program, file, offset.into()).expect("should resolve a context");
    let locals: Vec<_> = ctx
        .candidates
        .iter()
        .filter(|c| {
            matches!(
                c.kind,
                CompletionCandidateKind::Local | CompletionCandidateKind::Parameter
            )
        })
        .map(|c| c.label.as_str())
        .collect();

    assert!(locals.contains(&"x"), "param `x` should be visible: {locals:?}");
    assert!(locals.contains(&"y"), "outer local `y` should be visible: {locals:?}");
    assert!(locals.contains(&"z"), "innermost local `z` should be visible: {locals:?}");
}

#[test]
fn a_local_declared_after_the_cursor_is_not_yet_visible() {
    let (src, offset) = split_cursor(
        "public class Foo { \
             public void run() { \
                 |Integer later = 0; \
             } \
         }",
    );
    let dir = write_fixture_dir("completion-locals-not-yet-declared", &[("Foo.cls", &src)]);
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    let file = file_for_class(&program, "Foo");
    let ctx = complete_at(&program, file, offset.into()).expect("should resolve a context");
    let locals: Vec<_> = ctx
        .candidates
        .iter()
        .filter(|c| c.kind == CompletionCandidateKind::Local)
        .map(|c| c.label.as_str())
        .collect();
    assert!(
        !locals.contains(&"later"),
        "a not-yet-declared local should not be offered: {locals:?}"
    );
}

#[test]
fn enclosing_types_direct_members_are_offered_bare() {
    let (src, offset) = split_cursor(
        "public class Foo { \
             public Integer bar; \
             public void run() { | } \
         }",
    );
    let dir = write_fixture_dir("completion-direct-members-bare", &[("Foo.cls", &src)]);
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    let file = file_for_class(&program, "Foo");
    let ctx = complete_at(&program, file, offset.into()).expect("should resolve a context");
    let bar = ctx
        .candidates
        .iter()
        .find(|c| c.label == "bar")
        .expect("`bar` should be offered as a direct member");
    assert_eq!(bar.kind, CompletionCandidateKind::Field);
    assert!(!bar.is_inherited);

    let run = ctx
        .candidates
        .iter()
        .find(|c| c.label == "run")
        .expect("`run` (a Method) should be offered too, unlike bare-name *resolution*");
    assert_eq!(run.kind, CompletionCandidateKind::Method);
}

#[test]
fn an_overridden_method_appears_once_not_twice_and_is_not_flagged_inherited() {
    let (derived_src, offset) = split_cursor(
        "public class Derived extends Base { \
             public override void greet() { } \
             public void run() { | } \
         }",
    );
    let dir = write_fixture_dir(
        "completion-override-shadowing",
        &[
            (
                "Base.cls",
                "public virtual class Base { \
                     public virtual void greet() { } \
                     public void helper() { } \
                 }",
            ),
            ("Derived.cls", &derived_src),
        ],
    );
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    let file = file_for_class(&program, "Derived");
    let ctx = complete_at(&program, file, offset.into()).expect("should resolve a context");

    let greets: Vec<_> = ctx.candidates.iter().filter(|c| c.label == "greet").collect();
    assert_eq!(
        greets.len(),
        1,
        "an overridden method must appear once, not once per level: {:?}",
        labels_of(&ctx.candidates)
    );
    assert!(
        !greets[0].is_inherited,
        "the override itself lives on Derived, so it isn't inherited"
    );

    let helper = ctx
        .candidates
        .iter()
        .find(|c| c.label == "helper")
        .expect("a non-overridden inherited member should still be offered");
    assert!(helper.is_inherited, "helper is only declared on Base");
}

#[test]
fn member_access_off_a_project_typed_local_offers_its_members() {
    let (src, offset) = split_cursor(
        "public class Foo { \
             public Integer bar; \
             public void run() { Foo other = new Foo(); other.| } \
         }",
    );
    let dir = write_fixture_dir("completion-member-access-project", &[("Foo.cls", &src)]);
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    let file = file_for_class(&program, "Foo");
    let ctx = complete_at(&program, file, offset.into()).expect("should resolve a context");
    assert!(
        labels_of(&ctx.candidates).contains(&"bar"),
        "member-access off a project-typed receiver should offer its fields: {:?}",
        labels_of(&ctx.candidates)
    );
}

#[test]
fn member_access_off_a_stdlib_typed_local_offers_stdlib_members() {
    let (src, offset) = split_cursor(
        "public class Foo { \
             public void run() { String s = 'hi'; s.| } \
         }",
    );
    let dir = write_fixture_dir("completion-member-access-stdlib", &[("Foo.cls", &src)]);
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    let file = file_for_class(&program, "Foo");
    let ctx = complete_at(&program, file, offset.into()).expect("should resolve a context");
    assert!(
        !ctx.candidates.is_empty(),
        "a stdlib-typed receiver should offer its real scraped methods/properties"
    );
    assert!(
        labels_of(&ctx.candidates).contains(&"trim"),
        "String.trim should be a real, known stdlib method: {:?}",
        labels_of(&ctx.candidates)
    );
    assert!(ctx.candidates.iter().all(|c| matches!(
        c.kind,
        CompletionCandidateKind::StdlibMethod | CompletionCandidateKind::StdlibProperty
    )));
}

#[test]
fn member_access_off_an_sobject_typed_param_offers_schema_fields() {
    let (src, offset) = split_cursor(
        "public class Foo { \
             public void run(Account acct) { acct.| } \
         }",
    );
    let dir = write_fixture_dir("completion-member-access-sobject", &[("Foo.cls", &src)]);
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    let file = file_for_class(&program, "Foo");
    let ctx = complete_at(&program, file, offset.into()).expect("should resolve a context");
    assert!(
        labels_of(&ctx.candidates).contains(&"Name"),
        "Account.Name is a real standard field: {:?}",
        labels_of(&ctx.candidates)
    );
    assert!(ctx
        .candidates
        .iter()
        .all(|c| c.kind == CompletionCandidateKind::SObjectField));
}

#[test]
fn project_wide_types_and_keywords_appear_in_a_fresh_bare_identifier_context() {
    let (src, offset) = split_cursor("public class Foo { public void run() { | } }");
    let dir = write_fixture_dir(
        "completion-types-and-keywords",
        &[
            ("Foo.cls", &src),
            ("Bar.cls", "public class Bar { }"),
        ],
    );
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    let file = file_for_class(&program, "Foo");
    let ctx = complete_at(&program, file, offset.into()).expect("should resolve a context");
    let labels = labels_of(&ctx.candidates);
    assert!(labels.contains(&"Bar"), "another project type should be offered: {labels:?}");
    assert!(labels.contains(&"Foo"), "the enclosing type itself should be offered too: {labels:?}");
    assert!(labels.contains(&"if"), "a general keyword should be offered: {labels:?}");
    assert!(
        !labels.contains(&"select"),
        "a SOQL-only keyword must not be offered -- SOQL completion is out of v1 scope: {labels:?}"
    );
}

#[test]
fn a_private_member_of_an_unrelated_class_is_not_offered() {
    let (src, offset) = split_cursor(
        "public class Foo { \
             public void run(Other o) { o.| } \
         }",
    );
    let dir = write_fixture_dir(
        "completion-visibility-filtering",
        &[
            ("Foo.cls", &src),
            (
                "Other.cls",
                "public class Other { private Integer secret; public Integer visible; }",
            ),
        ],
    );
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    let file = file_for_class(&program, "Foo");
    let ctx = complete_at(&program, file, offset.into()).expect("should resolve a context");
    let labels = labels_of(&ctx.candidates);
    assert!(
        !labels.contains(&"secret"),
        "a private member of an unrelated class must not be offered: {labels:?}"
    );
    assert!(
        labels.contains(&"visible"),
        "a public member of the same receiver should still be offered: {labels:?}"
    );
}

#[test]
fn a_dangling_dot_replaces_an_empty_range_at_the_cursor() {
    let (src, offset) = split_cursor(
        "public class Foo { \
             public Integer bar; \
             public void run() { Foo other = new Foo(); other.| } \
         }",
    );
    let dir = write_fixture_dir("completion-replace-range-dangling", &[("Foo.cls", &src)]);
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    let file = file_for_class(&program, "Foo");
    let ctx = complete_at(&program, file, offset.into()).expect("should resolve a context");
    assert!(
        ctx.replace_range.is_empty(),
        "a dangling dot has nothing to overwrite yet: {:?}",
        ctx.replace_range
    );
    assert_eq!(u32::from(ctx.replace_range.start()), offset);
}

#[test]
fn a_partial_member_replaces_its_own_already_typed_span() {
    let (src, offset) = split_cursor(
        "public class Foo { \
             public Integer bar; \
             public void run() { Foo other = new Foo(); other.ba|r; } \
         }",
    );
    let dir = write_fixture_dir("completion-replace-range-partial", &[("Foo.cls", &src)]);
    let program = BoundProgram::from_files(&dir);
    std::fs::remove_dir_all(&dir).ok();

    let file = file_for_class(&program, "Foo");
    let ctx = complete_at(&program, file, offset.into()).expect("should resolve a context");
    assert_eq!(
        ctx.replace_range,
        rowan::TextRange::new(
            (offset - 2).into(), // start of `bar`
            (offset + 1).into(), // end of `bar`
        ),
        "should replace the whole already-typed member token, not just insert at the cursor"
    );
}
