//! Smoke-tests the typed AST layer (`apex_syntax::ast`) against every
//! real NPSP compilation unit: every declared name, and every statement/
//! expression accessor that's supposed to always resolve in an
//! error-free parse, must actually resolve. This is a different failure
//! mode than `whole_file_parse_rate.rs` (which only checks *parsing*
//! succeeds) -- a file can parse with zero errors while still producing
//! a tree shape the AST accessors can't walk, if a node ends up missing
//! or misplaced relative to what its accessor expects.
//!
//! Statement/expression accessors that are only *sometimes* present even
//! in a clean parse (`IfStmt::else_branch`, `MethodDecl::body` for an
//! abstract method, ...) are walked but not asserted `Some` -- only
//! fields that must exist whenever their enclosing node does (a
//! `BinExpr` always has both operands, a `CallExpr` always has a callee
//! token, ...) are asserted, so this stays a real regression guard
//! rather than an assertion that happens to pass today.

use apex_syntax::ast::decl::{
    CompilationUnit, ConstructorDecl, FieldDecl, Member, MethodDecl, PropertyDecl, TriggerUnit,
    TypeDecl,
};
use apex_syntax::ast::expr::Expr;
use apex_syntax::ast::stmt::Stmt;
use apex_syntax::AstNode;

fn is_known_non_compilation_unit(path: &std::path::Path) -> bool {
    let s = path.to_string_lossy().replace('\\', "/");
    s.contains("/scripts/") || s.ends_with("datasets/rd2/config_npsp_for_ldv_data_load.cls")
}

#[derive(Default)]
struct Counts {
    stmts: usize,
    exprs: usize,
}

#[test]
fn every_declared_name_and_body_resolves_through_the_ast_layer() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/corpus/npsp");
    let files = apex_discover::find_apex_files(&root);
    assert!(
        !files.is_empty(),
        "expected the NPSP submodule to be checked out"
    );

    let mut checked = 0usize;
    let mut counts = Counts::default();
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
                    check_member(path, member, &mut counts);
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
            check_type_decl(path, decl, &mut counts);
        }
        checked += 1;
    }
    assert!(
        checked > 1000,
        "expected to check over 1000 real NPSP files, only checked {checked}"
    );
    // Sanity floors on total reachable nodes -- if an accessor silently
    // stopped resolving (e.g. `Block::statements` returning nothing),
    // per-file assertions above might still pass on files that happen not
    // to exercise it, but the corpus-wide count would visibly collapse.
    assert!(
        counts.stmts > 50_000,
        "expected over 50k statements walked, found {}",
        counts.stmts
    );
    assert!(
        counts.exprs > 50_000,
        "expected over 50k expressions walked, found {}",
        counts.exprs
    );
}

fn check_type_decl(path: &std::path::Path, decl: TypeDecl, counts: &mut Counts) {
    match decl {
        TypeDecl::Class(class) => {
            assert!(
                class.name().is_some(),
                "{}: class has no name",
                path.display()
            );
            if let Some(body) = class.body() {
                for member in body.members() {
                    check_member(path, member, counts);
                }
            }
        }
        TypeDecl::Interface(iface) => {
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
        TypeDecl::Enum(en) => {
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

fn check_member(path: &std::path::Path, member: Member, counts: &mut Counts) {
    match member {
        Member::Method(method) => check_method(path, &method, counts),
        Member::Constructor(ctor) => check_constructor(path, &ctor, counts),
        Member::Field(field) => check_field(path, &field),
        Member::Property(prop) => check_property(path, &prop),
        Member::NestedClass(_) | Member::NestedInterface(_) | Member::NestedEnum(_) => {
            let decl = TypeDecl::cast(member.syntax().clone()).unwrap();
            check_type_decl(path, decl, counts);
        }
    }
}

fn check_method(path: &std::path::Path, method: &MethodDecl, counts: &mut Counts) {
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
    if let Some(body) = method.body() {
        walk_stmt(path, Stmt::Block(body), counts);
    }
}

fn check_constructor(path: &std::path::Path, ctor: &ConstructorDecl, counts: &mut Counts) {
    assert!(
        ctor.type_ref().is_some(),
        "{}: constructor has no type",
        path.display()
    );
    if let Some(body) = ctor.body() {
        walk_stmt(path, Stmt::Block(body), counts);
    }
}

fn check_field(path: &std::path::Path, field: &FieldDecl) {
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

fn check_property(path: &std::path::Path, prop: &PropertyDecl) {
    assert!(
        prop.name().is_some(),
        "{}: property has no name",
        path.display()
    );
}

fn walk_stmt(path: &std::path::Path, stmt: Stmt, counts: &mut Counts) {
    counts.stmts += 1;
    match stmt {
        Stmt::Block(b) => {
            for s in b.statements() {
                walk_stmt(path, s, counts);
            }
        }
        Stmt::If(s) => {
            let cond = s
                .condition()
                .unwrap_or_else(|| panic!("{}: if has no condition", path.display()));
            walk_expr(path, cond, counts);
            let then = s
                .then_branch()
                .unwrap_or_else(|| panic!("{}: if has no then-branch", path.display()));
            walk_stmt(path, then, counts);
            if let Some(e) = s.else_branch() {
                walk_stmt(path, e, counts);
            }
        }
        Stmt::Switch(s) => {
            if let Some(c) = s.condition() {
                walk_expr(path, c, counts);
            }
            for when in s.when_clauses() {
                if let Some(body) = when.body() {
                    walk_stmt(path, Stmt::Block(body), counts);
                }
            }
        }
        Stmt::For(s) => {
            if let Some(c) = s.condition() {
                walk_expr(path, c, counts);
            }
            if let Some(init) = s.init() {
                for e in init.exprs() {
                    walk_expr(path, e, counts);
                }
            }
            if let Some(update) = s.update() {
                for e in update.exprs() {
                    walk_expr(path, e, counts);
                }
            }
            if let Some(body) = s.body() {
                walk_stmt(path, body, counts);
            }
        }
        Stmt::ForEach(s) => {
            let iterable = s
                .iterable()
                .unwrap_or_else(|| panic!("{}: for-each has no iterable", path.display()));
            walk_expr(path, iterable, counts);
            if let Some(body) = s.body() {
                walk_stmt(path, body, counts);
            }
        }
        Stmt::While(s) => {
            let cond = s
                .condition()
                .unwrap_or_else(|| panic!("{}: while has no condition", path.display()));
            walk_expr(path, cond, counts);
            if let Some(body) = s.body() {
                walk_stmt(path, body, counts);
            }
        }
        Stmt::DoWhile(s) => {
            if let Some(body) = s.body() {
                walk_stmt(path, Stmt::Block(body), counts);
            }
            let cond = s
                .condition()
                .unwrap_or_else(|| panic!("{}: do-while has no condition", path.display()));
            walk_expr(path, cond, counts);
        }
        Stmt::Try(s) => {
            let body = s
                .body()
                .unwrap_or_else(|| panic!("{}: try has no body", path.display()));
            walk_stmt(path, Stmt::Block(body), counts);
            for catch in s.catch_clauses() {
                assert!(
                    catch.exception_type().is_some(),
                    "{}: catch has no exception type",
                    path.display()
                );
                assert!(
                    catch.name().is_some(),
                    "{}: catch has no bound name",
                    path.display()
                );
                if let Some(body) = catch.body() {
                    walk_stmt(path, Stmt::Block(body), counts);
                }
            }
            if let Some(fin) = s.finally_clause() {
                if let Some(body) = fin.body() {
                    walk_stmt(path, Stmt::Block(body), counts);
                }
            }
        }
        Stmt::Return(s) => {
            if let Some(e) = s.expr() {
                walk_expr(path, e, counts);
            }
        }
        Stmt::Throw(s) => {
            let e = s
                .expr()
                .unwrap_or_else(|| panic!("{}: throw has no expression", path.display()));
            walk_expr(path, e, counts);
        }
        Stmt::Break(_) | Stmt::Continue(_) => {}
        Stmt::Insert(s) => {
            walk_expr(
                path,
                s.expr()
                    .unwrap_or_else(|| panic!("{}: insert has no expression", path.display())),
                counts,
            );
        }
        Stmt::Update(s) => {
            walk_expr(
                path,
                s.expr()
                    .unwrap_or_else(|| panic!("{}: update has no expression", path.display())),
                counts,
            );
        }
        Stmt::Delete(s) => {
            walk_expr(
                path,
                s.expr()
                    .unwrap_or_else(|| panic!("{}: delete has no expression", path.display())),
                counts,
            );
        }
        Stmt::Undelete(s) => {
            walk_expr(
                path,
                s.expr()
                    .unwrap_or_else(|| panic!("{}: undelete has no expression", path.display())),
                counts,
            );
        }
        Stmt::Upsert(s) => {
            walk_expr(
                path,
                s.expr()
                    .unwrap_or_else(|| panic!("{}: upsert has no expression", path.display())),
                counts,
            );
        }
        Stmt::Merge(s) => {
            let master = s
                .master()
                .unwrap_or_else(|| panic!("{}: merge has no master expression", path.display()));
            walk_expr(path, master, counts);
            let dup = s
                .duplicate()
                .unwrap_or_else(|| panic!("{}: merge has no duplicate expression", path.display()));
            walk_expr(path, dup, counts);
        }
        Stmt::RunAs(s) => {
            for arg in s.args() {
                walk_expr(path, arg, counts);
            }
            if let Some(body) = s.body() {
                walk_stmt(path, Stmt::Block(body), counts);
            }
        }
        Stmt::LocalVarDecl(s) => {
            assert!(
                s.type_ref().is_some(),
                "{}: local var decl has no type",
                path.display()
            );
            let mut any = false;
            for decl in s.declarators() {
                any = true;
                assert!(
                    decl.name().is_some(),
                    "{}: local var declarator has no name",
                    path.display()
                );
                if let Some(init) = decl.init() {
                    walk_expr(path, init, counts);
                }
            }
            assert!(any, "{}: local var decl has no declarators", path.display());
        }
        Stmt::Expr(s) => {
            if let Some(e) = s.expr() {
                walk_expr(path, e, counts);
            }
        }
    }
}

fn walk_expr(path: &std::path::Path, expr: Expr, counts: &mut Counts) {
    counts.exprs += 1;
    match expr {
        Expr::Literal(_) | Expr::This(_) | Expr::Super(_) | Expr::Soql(_) | Expr::Sosl(_) => {}
        Expr::Name(e) => {
            assert!(
                e.name_token().is_some() || e.type_ref().is_some(),
                "{}: name expr has neither a token nor a type",
                path.display()
            );
        }
        Expr::Paren(e) => {
            let inner = e
                .inner()
                .unwrap_or_else(|| panic!("{}: paren expr has no inner expr", path.display()));
            walk_expr(path, inner, counts);
        }
        Expr::Cast(e) => {
            assert!(
                e.type_ref().is_some(),
                "{}: cast has no type",
                path.display()
            );
            let operand = e
                .operand()
                .unwrap_or_else(|| panic!("{}: cast has no operand", path.display()));
            walk_expr(path, operand, counts);
        }
        Expr::Bin(e) => {
            let lhs = e
                .lhs()
                .unwrap_or_else(|| panic!("{}: bin expr has no lhs", path.display()));
            walk_expr(path, lhs, counts);
            assert!(
                !e.operator_tokens().is_empty(),
                "{}: bin expr has no operator",
                path.display()
            );
            let rhs = e
                .rhs()
                .unwrap_or_else(|| panic!("{}: bin expr has no rhs", path.display()));
            walk_expr(path, rhs, counts);
        }
        Expr::Unary(e) => {
            assert!(
                e.operator().is_some(),
                "{}: unary expr has no operator",
                path.display()
            );
            let operand = e
                .operand()
                .unwrap_or_else(|| panic!("{}: unary expr has no operand", path.display()));
            walk_expr(path, operand, counts);
        }
        Expr::Postfix(e) => {
            let operand = e
                .operand()
                .unwrap_or_else(|| panic!("{}: postfix expr has no operand", path.display()));
            walk_expr(path, operand, counts);
            assert!(
                e.operator().is_some(),
                "{}: postfix expr has no operator",
                path.display()
            );
        }
        Expr::Ternary(e) => {
            let cond = e
                .condition()
                .unwrap_or_else(|| panic!("{}: ternary has no condition", path.display()));
            walk_expr(path, cond, counts);
            let then = e
                .then_branch()
                .unwrap_or_else(|| panic!("{}: ternary has no then-branch", path.display()));
            walk_expr(path, then, counts);
            let els = e
                .else_branch()
                .unwrap_or_else(|| panic!("{}: ternary has no else-branch", path.display()));
            walk_expr(path, els, counts);
        }
        Expr::Instanceof(e) => {
            let operand = e
                .operand()
                .unwrap_or_else(|| panic!("{}: instanceof has no operand", path.display()));
            walk_expr(path, operand, counts);
            assert!(
                e.type_ref().is_some(),
                "{}: instanceof has no type",
                path.display()
            );
        }
        Expr::Field(e) => {
            let target = e
                .target()
                .unwrap_or_else(|| panic!("{}: field expr has no target", path.display()));
            walk_expr(path, target, counts);
            assert!(
                e.member_token().is_some(),
                "{}: field expr has no member token",
                path.display()
            );
        }
        Expr::Index(e) => {
            let target = e
                .target()
                .unwrap_or_else(|| panic!("{}: index expr has no target", path.display()));
            walk_expr(path, target, counts);
            if let Some(index) = e.index() {
                walk_expr(path, index, counts);
            }
        }
        Expr::Call(e) => {
            assert!(
                e.callee_token().is_some(),
                "{}: call expr has no callee",
                path.display()
            );
            for arg in e.args().into_iter().flat_map(|a| a.args()) {
                walk_expr(path, arg, counts);
            }
        }
        Expr::MethodCall(e) => {
            let target = e
                .target()
                .unwrap_or_else(|| panic!("{}: method call has no target", path.display()));
            walk_expr(path, target, counts);
            assert!(
                e.method_name_token().is_some(),
                "{}: method call has no method name",
                path.display()
            );
            for arg in e.args().into_iter().flat_map(|a| a.args()) {
                walk_expr(path, arg, counts);
            }
        }
        Expr::New(e) => {
            assert!(
                e.type_ref().is_some(),
                "{}: new expr has no type",
                path.display()
            );
            for arg in e.args().into_iter().flat_map(|a| a.args()) {
                walk_expr(path, arg, counts);
            }
            if let Some(size) = e.array_size() {
                walk_expr(path, size, counts);
            }
            if let Some(init) = e.initializer() {
                walk_initializer(path, init, counts);
            }
        }
    }
}

fn walk_initializer(
    path: &std::path::Path,
    init: apex_syntax::ast::expr::Initializer,
    counts: &mut Counts,
) {
    use apex_syntax::ast::expr::Initializer;
    match init {
        Initializer::Array(a) => {
            for e in a.elements() {
                walk_expr(path, e, counts);
            }
        }
        Initializer::Set(s) => {
            for e in s.elements() {
                walk_expr(path, e, counts);
            }
        }
        Initializer::Map(m) => {
            for entry in m.entries() {
                let key = entry
                    .key()
                    .unwrap_or_else(|| panic!("{}: map entry has no key", path.display()));
                walk_expr(path, key, counts);
                let value = entry
                    .value()
                    .unwrap_or_else(|| panic!("{}: map entry has no value", path.display()));
                walk_expr(path, value, counts);
            }
        }
    }
}
