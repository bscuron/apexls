//! Smoke-tests the typed AST layer (`apex_syntax::ast::decl`) against
//! every real NPSP compilation unit: every declared name the grammar is
//! supposed to expose via a `Name`/`DeclName` node must actually resolve
//! through the corresponding accessor. This is a different failure mode
//! than `whole_file_parse_rate.rs` (which only checks *parsing*
//! succeeds) -- a file can parse with zero errors while still producing
//! a tree shape the AST accessors can't walk, if a declaration's `Name`
//! node ends up missing or misplaced.

use apex_syntax::ast::decl::{
    ClassDecl, CompilationUnit, EnumDecl, InterfaceDecl, Member, TriggerUnit, TypeDecl,
};
use apex_syntax::AstNode;

fn is_known_non_compilation_unit(path: &std::path::Path) -> bool {
    let s = path.to_string_lossy().replace('\\', "/");
    s.contains("/scripts/") || s.ends_with("datasets/rd2/config_npsp_for_ldv_data_load.cls")
}

#[test]
fn every_declared_name_resolves_through_the_ast_layer() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/corpus/npsp");
    let files = apex_discover::find_apex_files(&root);
    assert!(
        !files.is_empty(),
        "expected the NPSP submodule to be checked out"
    );

    let mut checked = 0usize;
    for path in &files {
        if is_known_non_compilation_unit(path) {
            continue;
        }
        let src =
            std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        let is_trigger = path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("trigger"));

        if is_trigger {
            let parse = apex_parser::parse_trigger_unit(&src);
            assert!(
                parse.errors.is_empty(),
                "{}: {:?}",
                path.display(),
                parse.errors
            );
            let trigger = TriggerUnit::cast(parse.syntax())
                .unwrap_or_else(|| panic!("{}: root isn't a TriggerUnit", path.display()));
            assert!(
                trigger.name().is_some(),
                "{}: trigger has no name",
                path.display()
            );
            assert!(
                trigger.object_ref().is_some(),
                "{}: trigger has no object reference",
                path.display()
            );
            if let Some(block) = trigger.block() {
                for member in block.members() {
                    check_member(path, member);
                }
            }
        } else {
            let parse = apex_parser::parse_compilation_unit(&src);
            assert!(
                parse.errors.is_empty(),
                "{}: {:?}",
                path.display(),
                parse.errors
            );
            let cu = CompilationUnit::cast(parse.syntax())
                .unwrap_or_else(|| panic!("{}: root isn't a CompilationUnit", path.display()));
            let decl = cu.type_decl().unwrap_or_else(|| {
                panic!(
                    "{}: compilation unit has no type declaration",
                    path.display()
                )
            });
            check_type_decl(path, decl);
        }
        checked += 1;
    }
    assert!(
        checked > 1000,
        "expected to check over 1000 real NPSP files, only checked {checked}"
    );
}

fn check_type_decl(path: &std::path::Path, decl: TypeDecl) {
    match decl {
        TypeDecl::Class(node) => {
            let class = ClassDecl::cast(node).unwrap();
            assert!(
                class.name().is_some(),
                "{}: class has no name",
                path.display()
            );
            if let Some(body) = class.body() {
                for member in body.members() {
                    check_member(path, member);
                }
            }
        }
        TypeDecl::Interface(node) => {
            let iface = InterfaceDecl::cast(node).unwrap();
            assert!(
                iface.name().is_some(),
                "{}: interface has no name",
                path.display()
            );
            if let Some(body) = iface.body() {
                for method in body.methods() {
                    assert!(
                        method.name().is_some(),
                        "{}: interface method has no name",
                        path.display()
                    );
                }
            }
        }
        TypeDecl::Enum(node) => {
            let en = EnumDecl::cast(node).unwrap();
            assert!(en.name().is_some(), "{}: enum has no name", path.display());
            for constant in en.constant_list().into_iter().flat_map(|l| l.constants()) {
                assert!(
                    constant.text().is_some_and(|t| !t.is_empty()),
                    "{}: enum constant has no text",
                    path.display()
                );
            }
        }
    }
}

fn check_member(path: &std::path::Path, member: Member) {
    match member {
        Member::Method(node) => {
            let method = apex_syntax::ast::decl::MethodDecl::cast(node).unwrap();
            assert!(
                method.name().is_some(),
                "{}: method has no name",
                path.display()
            );
            for param in method.params().into_iter().flat_map(|l| l.params()) {
                assert!(
                    param.name().is_some(),
                    "{}: parameter has no name",
                    path.display()
                );
            }
        }
        Member::Constructor(node) => {
            let ctor = apex_syntax::ast::decl::ConstructorDecl::cast(node).unwrap();
            assert!(
                ctor.type_ref().is_some(),
                "{}: constructor has no type",
                path.display()
            );
        }
        Member::Field(node) => {
            let field = apex_syntax::ast::decl::FieldDecl::cast(node).unwrap();
            assert!(
                field.type_ref().is_some(),
                "{}: field has no type",
                path.display()
            );
            let mut any = false;
            for decl in field.declarators() {
                any = true;
                assert!(
                    decl.name().is_some(),
                    "{}: field declarator has no name",
                    path.display()
                );
            }
            assert!(any, "{}: field has no declarators", path.display());
        }
        Member::Property(node) => {
            let prop = apex_syntax::ast::decl::PropertyDecl::cast(node).unwrap();
            assert!(
                prop.name().is_some(),
                "{}: property has no name",
                path.display()
            );
        }
        Member::NestedClass(_) | Member::NestedInterface(_) | Member::NestedEnum(_) => {
            let decl = TypeDecl::cast(member.syntax().clone()).unwrap();
            check_type_decl(path, decl);
        }
    }
}
