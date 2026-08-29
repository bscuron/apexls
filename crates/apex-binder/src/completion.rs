//! `textDocument/completion`'s pure analysis layer: given a byte offset in
//! a bound file, figures out what completion context the cursor is in
//! (member-access after a `.`/`?.`, or a bare identifier/fresh position)
//! and enumerates candidates -- locals in scope, a type's own and
//! inherited members, project-wide top-level types, stdlib classes/
//! members, SObject schema fields, and a small set of general keywords.
//! Protocol-agnostic (no `lsp_types` dependency, matching every other
//! capability module here): `apexls-server::capabilities::completion` is
//! the thin layer that turns this into an LSP `CompletionResponse`.
//!
//! v1 scope (see `BACKLOG.md`'s completion entry): member-access and
//! bare-identifier contexts only -- no SOQL/SOSL completion, no
//! snippets, no auto-import, no `completionItem/resolve` (documentation
//! is deliberately not fetched per candidate -- see
//! [`CompletionCandidate`]'s own doc comment). Filtering by whatever
//! prefix the user has already typed is deliberately left to the
//! client's own fuzzy matcher (standard LSP architecture): this always
//! returns the *full* candidate set for the resolved context, plus a
//! `replace_range` so accepting an item overwrites rather than
//! duplicates any partial text already there.

use crate::resolve::body_binder_for_completion;
use crate::schema_index::SchemaIndex;
use crate::scope::{ScopeId, ScopeKind, ScopeTree};
use crate::stdlib_index::StdlibIndex;
use crate::symbol::{SymbolId, SymbolKind};
use crate::symbol_table::SymbolTable;
use crate::ty::Ty;
use crate::{BoundProgram, FileId, SyntaxPtr};
use apex_syntax::ast::decl::{ConstructorDecl, MethodDecl, PropertyAccessor, TriggerUnit};
use apex_syntax::ast::expr::{FieldExpr, MethodCallExpr};
use apex_syntax::{AstNode, SyntaxKind, SyntaxNode, SyntaxToken};
use rowan::{TextRange, TextSize};
use rustc_hash::FxHashSet;
use smol_str::SmolStr;

/// The resolved completion context: every candidate for the cursor
/// position, plus the range accepting one should replace (empty at the
/// cursor when nothing's typed yet -- a dangling `foo.` or a fresh
/// statement position -- otherwise the already-typed partial token's own
/// span, so accepting a candidate overwrites it rather than duplicating
/// it).
pub struct CompletionContext {
    pub candidates: Vec<CompletionCandidate>,
    pub replace_range: TextRange,
}

/// One completion candidate. Deliberately carries no `documentation`
/// field: fetching a doc comment is an extra tree walk per candidate, for
/// every candidate, on every keystroke -- expensive even done eagerly,
/// unlike everything else here (bounded by scope depth or one type's
/// member count). A field/method's hover already gives full doc text
/// once a candidate is actually inserted and hovered, so nothing is lost
/// permanently, only deferred to a query this server already answers
/// well.
///
/// `symbol`/`stdlib_class`/`sobject_field` are provided (rather than a
/// pre-formatted display string) so `apexls-server::capabilities` can
/// build a `CompletionItem::detail` by reusing its own existing
/// hover/signature formatters end-to-end instead of a second formatter
/// living in this protocol-agnostic crate.
pub struct CompletionCandidate {
    pub label: SmolStr,
    pub kind: CompletionCandidateKind,
    /// A member reached through `inherited_chain` rather than declared
    /// directly on the receiver/enclosing type -- drives the server
    /// layer's `sort_text` tiering (direct members rank above inherited
    /// ones). Meaningless (`false`) for every non-member kind.
    pub is_inherited: bool,
    /// Set for every project-local kind (`Local`/`Parameter`/`Field`/
    /// `Property`/`Method`/`Constructor`/`EnumConstant`/`Class`/
    /// `Interface`/`Enum`).
    pub symbol: Option<SymbolId>,
    /// Set for `StdlibClass`/`StdlibMethod`/`StdlibProperty` -- the
    /// declaring class's own name (not `label`, which is the member's).
    pub stdlib_class: Option<SmolStr>,
    /// Set for `SObjectField` -- `(object api name, field api name)`.
    pub sobject_field: Option<(SmolStr, SmolStr)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompletionCandidateKind {
    Local,
    Parameter,
    Field,
    Property,
    Method,
    Constructor,
    EnumConstant,
    Class,
    Interface,
    Enum,
    StdlibClass,
    StdlibMethod,
    StdlibProperty,
    SObjectField,
    Keyword,
}

/// General statement/expression keywords only -- deliberately *not*
/// `apex_lexer::keyword::KEYWORDS`, which also covers ~150 SOQL/SOSL/
/// date-literal keywords (`select`, `from`, `next_n_fiscal_quarters`,
/// ...) that would be actively wrong to suggest in plain statement/
/// expression position, since SOQL completion is out of v1 scope (see
/// this module's own doc comment).
const GENERAL_KEYWORDS: &[&str] = &[
    "if", "else", "for", "while", "do", "switch", "when", "try", "catch", "finally", "return",
    "new", "this", "super", "null", "true", "false", "break", "continue", "throw", "instanceof",
    "class", "interface", "enum", "trigger", "public", "private", "protected", "global",
    "static", "final", "override", "virtual", "abstract", "void", "get", "set", "insert",
    "update", "upsert", "delete", "undelete", "merge",
];

/// The entry point: what should complete at `offset` in `file`, if
/// anything. `None` only when `offset` lands nowhere sensible at all
/// (an empty file, or a position rowan's tree can't resolve a token for).
pub fn complete_at(program: &BoundProgram, file: FileId, offset: TextSize) -> Option<CompletionContext> {
    let root = program.syntax(file);
    let token = resolve_token_at(&root, offset)?;

    // A dangling `.`/`?.` with nothing (id-shaped) typed after it yet --
    // `expr_primary_chain` still completes a `FieldExpr`/`MethodCallExpr`
    // around it (see `apex_syntax::ast::expr::FieldExpr::member_token`'s
    // doc comment), so the dot's own direct parent is exactly that node.
    if matches!(token.kind(), SyntaxKind::Dot | SyntaxKind::QuestionDot) {
        let node = token.parent()?;
        return member_access_context(program, file, &node, TextRange::empty(offset));
    }

    // A real (possibly keyword-shaped, per `anyId`) token that's exactly
    // an already-parsed `FieldExpr`/`MethodCallExpr`'s member/method-name
    // token -- completing/replacing an already-typed partial member
    // (`foo.b|`).
    if let Some(field) = ancestor_field_expr_with_member(&token) {
        return member_access_context(program, file, field.syntax(), token.text_range());
    }
    if let Some(call) = ancestor_method_call_with_name(&token) {
        return member_access_context(program, file, call.syntax(), token.text_range());
    }

    // Otherwise: a bare identifier being typed (`de|`), or a fresh
    // position with nothing typed at all (right after `;`/`{`, say).
    let start = token.parent()?;
    let range = if token.kind() == SyntaxKind::Identifier {
        token.text_range()
    } else {
        TextRange::empty(offset)
    };
    Some(bare_identifier_context(program, file, &start, range))
}

/// Mirrors `BoundProgram::resolution_at`'s `TokenAtOffset::Between`
/// disambiguation: whichever side is an actual `Identifier` token wins,
/// since that's unambiguously the name the cursor is "on." Falls back to
/// the same trivia-based rule for the (practically unreachable for an
/// identifier boundary) case where neither or both sides are
/// `Identifier` tokens -- see `resolution_at`'s own doc comment for why.
fn resolve_token_at(root: &SyntaxNode, offset: TextSize) -> Option<SyntaxToken> {
    match root.token_at_offset(offset) {
        rowan::TokenAtOffset::None => None,
        rowan::TokenAtOffset::Single(t) => Some(t),
        rowan::TokenAtOffset::Between(left, right) => {
            match (
                left.kind() == SyntaxKind::Identifier,
                right.kind() == SyntaxKind::Identifier,
            ) {
                (true, false) => Some(left),
                (false, true) => Some(right),
                _ => Some(if left.kind().is_trivia() { right } else { left }),
            }
        }
    }
}

fn ancestor_field_expr_with_member(token: &SyntaxToken) -> Option<FieldExpr> {
    let field = FieldExpr::cast(token.parent()?)?;
    let member = field.member_token()?;
    (member.text_range() == token.text_range()).then_some(field)
}

fn ancestor_method_call_with_name(token: &SyntaxToken) -> Option<MethodCallExpr> {
    let call = MethodCallExpr::cast(token.parent()?)?;
    let name = call.method_name_token()?;
    (name.text_range() == token.text_range()).then_some(call)
}

fn member_access_context(
    program: &BoundProgram,
    file: FileId,
    node: &SyntaxNode,
    replace_range: TextRange,
) -> Option<CompletionContext> {
    let target = FieldExpr::cast(node.clone())
        .and_then(|f| f.target())
        .or_else(|| MethodCallExpr::cast(node.clone()).and_then(|m| m.target()))?;

    let enclosing = enclosing_context(program, file, node, replace_range.start());
    let mut binder = body_binder_for_completion(
        &program.symbols,
        &program.schema,
        &program.stdlib,
        &program.labels,
        file,
        enclosing.enclosing_type,
        enclosing.enclosing_member,
        enclosing.scopes,
    );
    let ty = binder.bind_expr(enclosing.scope, &target);

    let candidates = match ty {
        Some(Ty::Project(container)) => {
            member_candidates(&program.symbols, container, enclosing.enclosing_type)
        }
        Some(Ty::System { name, .. }) => {
            system_member_candidates(&program.schema, &program.stdlib, &name)
        }
        None => Vec::new(),
    };
    Some(CompletionContext {
        candidates,
        replace_range,
    })
}

fn bare_identifier_context(
    program: &BoundProgram,
    file: FileId,
    start: &SyntaxNode,
    replace_range: TextRange,
) -> CompletionContext {
    let enclosing = enclosing_context(program, file, start, replace_range.start());
    let mut candidates = Vec::new();

    // Locals/params, closest scope first. `Scope::bindings` records every
    // local ever declared in that scope regardless of position (nothing
    // about a `ScopeTree` needs position-ordering for its one existing
    // use, `resolve_local`, which is only ever asked about a reference
    // that's already textually valid) -- filtered here to the ones whose
    // own declared name actually precedes the cursor, so a not-yet-typed
    // `Integer later = 0;` further down the same block isn't offered as
    // though it were already in scope.
    let mut current = Some(enclosing.scope);
    while let Some(id) = current {
        let scope = enclosing.scopes.scope(id);
        for (name, sym_id) in scope.bindings() {
            let sym = program.symbols.get(*sym_id);
            if sym.name_range.start() >= replace_range.start() {
                continue;
            }
            candidates.push(CompletionCandidate {
                label: name.clone(),
                kind: candidate_kind_for_symbol(sym.kind),
                is_inherited: false,
                symbol: Some(*sym_id),
                stdlib_class: None,
                sobject_field: None,
            });
        }
        current = scope.parent;
    }

    // The enclosing type's own + inherited members.
    if let Some(enclosing_type) = enclosing.enclosing_type {
        candidates.extend(member_candidates(
            &program.symbols,
            enclosing_type,
            enclosing.enclosing_type,
        ));
    }

    // Project-wide top-level types -- the same full-project-scan-is-fine
    // pattern `capabilities::workspace_symbols` already uses.
    candidates.extend(
        program
            .symbols
            .iter()
            .filter(|(_, s)| s.kind.is_type())
            .map(|(id, s)| CompletionCandidate {
                label: s.name.clone(),
                kind: candidate_kind_for_symbol(s.kind),
                is_inherited: false,
                symbol: Some(id),
                stdlib_class: None,
                sobject_field: None,
            }),
    );

    // Stdlib top-level classes.
    candidates.extend(
        apex_stdlib::standard_classes()
            .iter()
            .map(|c| CompletionCandidate {
                label: c.name.clone(),
                kind: CompletionCandidateKind::StdlibClass,
                is_inherited: false,
                symbol: None,
                stdlib_class: Some(c.name.clone()),
                sobject_field: None,
            }),
    );

    // General keywords.
    candidates.extend(GENERAL_KEYWORDS.iter().map(|kw| CompletionCandidate {
        label: SmolStr::new_static(kw),
        kind: CompletionCandidateKind::Keyword,
        is_inherited: false,
        symbol: None,
        stdlib_class: None,
        sobject_field: None,
    }));

    CompletionContext {
        candidates,
        replace_range,
    }
}

/// Every visible member of `container` (direct, per `SymbolTable::members_of`,
/// plus every ancestor in `SymbolTable::inherited_chain`), override-
/// shadowed and deduplicated the same way a real `foo.bar()` call site
/// resolves: `SymbolTable::lookup_member` is the authority for *which*
/// symbol a given name resolves to (it already handles arity-aware
/// override shadowing), so this only uses direct/inherited member
/// enumeration to discover the *distinct names* to look up, then asks
/// `lookup_member` once per distinct name against the original
/// `container` (never a `chain` entry directly) so the answer is
/// identical regardless of which ancestor first introduced that name.
///
/// Deliberately does *not* exclude `Method`/`Constructor` the way
/// `resolve::bind_name_expr`'s bare-name *resolution* does for an
/// unqualified reference -- completion should still suggest `getName()`
/// as a candidate even though typing the bare name `getName` alone would
/// never resolve to it in real code.
fn member_candidates(
    table: &SymbolTable,
    container: SymbolId,
    visible_from: Option<SymbolId>,
) -> Vec<CompletionCandidate> {
    let mut seen: FxHashSet<String> = FxHashSet::default();
    let mut out = Vec::new();
    let chain = std::iter::once(container).chain(table.inherited_chain(container).iter().copied());
    for type_id in chain {
        for &member_id in table.members_of(type_id) {
            let name = &table.get(member_id).name;
            if !seen.insert(name.to_ascii_lowercase()) {
                continue;
            }
            for id in table.lookup_member(container, name) {
                if !table.is_visible_from(id, visible_from) {
                    continue;
                }
                let sym = table.get(id);
                out.push(CompletionCandidate {
                    label: sym.name.clone(),
                    kind: candidate_kind_for_symbol(sym.kind),
                    is_inherited: sym.container != Some(container),
                    symbol: Some(id),
                    stdlib_class: None,
                    sobject_field: None,
                });
            }
        }
    }
    out
}

/// Member candidates for a non-project (`Ty::System`) receiver: an
/// SObject's schema fields take priority (mirrors
/// `resolve::bind_field_expr`'s own schema-before-stdlib order), falling
/// back to a stdlib class's methods/properties. Neither ever needs a new
/// `SchemaIndex`/`StdlibIndex` API: `SObjectSchema::fields`/
/// `StdlibClass::methods`/`::properties` are already public, directly
/// enumerable `Vec`s once the concrete type name is known.
fn system_member_candidates(
    schema: &SchemaIndex,
    stdlib: &StdlibIndex,
    type_name: &str,
) -> Vec<CompletionCandidate> {
    if let Some(object) = schema.object(type_name) {
        return object
            .fields
            .iter()
            .map(|f| CompletionCandidate {
                label: f.api_name.clone(),
                kind: CompletionCandidateKind::SObjectField,
                is_inherited: false,
                symbol: None,
                stdlib_class: None,
                sobject_field: Some((object.api_name.clone(), f.api_name.clone())),
            })
            .collect();
    }
    let Some(class) = stdlib.class(type_name) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for m in &class.methods {
        // Constructors are scraped under `methods` with the class's own
        // name (see `apex_stdlib::StdlibMethod`'s doc comment) -- not
        // reachable via member access (`x.String(...)` isn't valid Apex),
        // so they're not real candidates here.
        if m.name.eq_ignore_ascii_case(&class.name) {
            continue;
        }
        out.push(CompletionCandidate {
            label: m.name.clone(),
            kind: CompletionCandidateKind::StdlibMethod,
            is_inherited: false,
            symbol: None,
            stdlib_class: Some(class.name.clone()),
            sobject_field: None,
        });
    }
    for p in &class.properties {
        out.push(CompletionCandidate {
            label: p.name.clone(),
            kind: CompletionCandidateKind::StdlibProperty,
            is_inherited: false,
            symbol: None,
            stdlib_class: Some(class.name.clone()),
            sobject_field: None,
        });
    }
    out
}

fn candidate_kind_for_symbol(kind: SymbolKind) -> CompletionCandidateKind {
    match kind {
        SymbolKind::Class => CompletionCandidateKind::Class,
        SymbolKind::Interface => CompletionCandidateKind::Interface,
        SymbolKind::Enum => CompletionCandidateKind::Enum,
        SymbolKind::EnumConstant => CompletionCandidateKind::EnumConstant,
        SymbolKind::Method => CompletionCandidateKind::Method,
        SymbolKind::Constructor => CompletionCandidateKind::Constructor,
        SymbolKind::Field => CompletionCandidateKind::Field,
        SymbolKind::Property => CompletionCandidateKind::Property,
        SymbolKind::Parameter => CompletionCandidateKind::Parameter,
        SymbolKind::LocalVar
        | SymbolKind::CatchVar
        | SymbolKind::ForEachVar
        | SymbolKind::SwitchBindingVar => CompletionCandidateKind::Local,
        // Never actually produced by any candidate source above: a
        // trigger is never a member (`SymbolKind::is_type` excludes it
        // from the top-level-type scan too), so this arm exists only to
        // satisfy exhaustiveness.
        SymbolKind::Trigger => CompletionCandidateKind::Class,
    }
}

struct EnclosingContext {
    scope: ScopeId,
    scopes: ScopeTree,
    enclosing_type: Option<SymbolId>,
    enclosing_member: Option<SymbolId>,
}

/// Walks `start`'s ancestors to find: the real per-body `ScopeTree`
/// belonging to whichever method/constructor/accessor/trigger body
/// contains `start` (cloned -- see `resolve::body_binder_for_completion`'s
/// doc comment for why that's cheap and safe), and the `enclosing_type`/
/// `enclosing_member` that body was originally bound with. Mirrors
/// `apex_binder::bind_symbol_body`'s own per-declaration-kind dispatch,
/// but structurally (via `Symbol::ptr` equality against whatever
/// declaration node the climb actually reaches) rather than by
/// duplicating its kind-by-kind branching -- correct for any nesting
/// depth (a nested class's method, a trigger's top-level body, ...)
/// without needing to special-case each one here.
///
/// Falls back to a fresh, empty root `ScopeTree` (no locals, matching
/// `resolve::bind_initializer`'s own shape) when `start` isn't inside any
/// body at all -- a field/property initializer expression, which is
/// never wrapped in a `Block`.
fn enclosing_context(
    program: &BoundProgram,
    file: FileId,
    start: &SyntaxNode,
    offset: TextSize,
) -> EnclosingContext {
    let mut body_scopes: Option<ScopeTree> = None;
    let mut enclosing_member: Option<SymbolId> = None;
    let mut enclosing_type: Option<SymbolId> = None;

    for ancestor in start.ancestors() {
        match ancestor.kind() {
            SyntaxKind::MethodDecl if body_scopes.is_none() => {
                if let Some(m) = MethodDecl::cast(ancestor.clone()) {
                    if let Some(sym) = find_symbol_by_ptr(program, file, SyntaxPtr::new(file, &ancestor)) {
                        enclosing_member = Some(sym);
                        body_scopes = m
                            .body()
                            .and_then(|b| program.scope_tree(SyntaxPtr::new(file, b.syntax())))
                            .cloned();
                    }
                }
            }
            SyntaxKind::ConstructorDecl if body_scopes.is_none() => {
                if let Some(c) = ConstructorDecl::cast(ancestor.clone()) {
                    if let Some(sym) = find_symbol_by_ptr(program, file, SyntaxPtr::new(file, &ancestor)) {
                        enclosing_member = Some(sym);
                        body_scopes = c
                            .body()
                            .and_then(|b| program.scope_tree(SyntaxPtr::new(file, b.syntax())))
                            .cloned();
                    }
                }
            }
            // An accessor body's `enclosing_member` is unconditionally
            // `None`, matching `bind_symbol_body`'s own
            // `SymbolKind::Property` arm exactly.
            SyntaxKind::PropertyAccessor if body_scopes.is_none() => {
                if let Some(a) = PropertyAccessor::cast(ancestor.clone()) {
                    body_scopes = a
                        .body()
                        .and_then(|b| program.scope_tree(SyntaxPtr::new(file, b.syntax())))
                        .cloned();
                }
            }
            // A trigger is never nested inside a further container, so
            // this is always the last ancestor worth inspecting. Its own
            // `SymbolId` becomes `enclosing_type` directly (not climbed
            // to further, and not via a `Class`/`Interface`/`Enum`
            // ancestor, since there isn't one) -- matches
            // `bind_symbol_body`'s `SymbolKind::Trigger` arm, which
            // passes the trigger's own id through as `enclosing_type` so
            // any top-level trigger `Member` declarations remain
            // reachable via unqualified lookup.
            SyntaxKind::TriggerUnit => {
                if let Some(tu) = TriggerUnit::cast(ancestor.clone()) {
                    if let Some(sym) = find_symbol_by_ptr(program, file, SyntaxPtr::new(file, &ancestor)) {
                        if body_scopes.is_none() {
                            body_scopes = tu
                                .block()
                                .and_then(|b| program.scope_tree(SyntaxPtr::new(file, b.syntax())))
                                .cloned();
                        }
                        enclosing_type = Some(sym);
                    }
                }
                break;
            }
            SyntaxKind::ClassDecl | SyntaxKind::InterfaceDecl | SyntaxKind::EnumDecl => {
                enclosing_type = find_symbol_by_ptr(program, file, SyntaxPtr::new(file, &ancestor));
                break;
            }
            _ => {}
        }
    }

    let (scopes, scope) = match body_scopes {
        Some(tree) => {
            let scope = tree
                .scope_at(TextRange::empty(offset))
                // Every `ScopeTree` is built via `ScopeTree::new_root`,
                // whose very first `push` always allocates id 0 for the
                // body's own root scope -- a safe, structural fallback,
                // not a guess, for the (practically unreachable, since
                // `start` is always inside the body whose tree this is)
                // case `scope_at` finds nothing narrower.
                .unwrap_or(ScopeId(0));
            (tree, scope)
        }
        None => ScopeTree::new_root(ScopeKind::Body, start.text_range()),
    };

    EnclosingContext {
        scope,
        scopes,
        enclosing_type,
        enclosing_member,
    }
}

fn find_symbol_by_ptr(program: &BoundProgram, file: FileId, ptr: SyntaxPtr) -> Option<SymbolId> {
    program
        .symbols
        .symbols_of_file(file)
        .iter()
        .position(|s| s.ptr == ptr)
        .map(|local| SymbolId::new(file, local as u32))
}
