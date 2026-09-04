//! Shared plumbing for position-based capabilities (`textDocument/hover`,
//! `textDocument/definition`, and whatever else `BACKLOG.md` §3 adds
//! next): resolving an LSP `(Url, Position)` down to the coordinates
//! `apex_binder::BoundProgram`'s own API understands, and the reverse --
//! turning a resolved `SymbolId` back into an LSP `Location`.

use crate::line_index::{LineIndex, PositionEncoding};
use apex_binder::{
    BoundProgram, CompletionCandidate, CompletionCandidateKind, FileId, LabelRef, Resolution,
    SchemaObjectRef, StdlibMemberRef, Symbol, SymbolId, SymbolKind, SyntaxPtr, Visibility,
    VisualforcePageRef,
};
use apex_syntax::ast::decl::{
    ClassDecl, ConstructorDecl, EnumDecl, FieldDecl, FormalParam, FormalParamList, HasDocComment,
    InterfaceDecl, MethodDecl, Modifier, PropertyDecl, TriggerUnit,
};
use apex_syntax::ast::expr::{ArgList, CallExpr, Expr, MethodCallExpr, NameExpr, NewExpr};
use apex_syntax::ast::stmt::{Block, DoWhileStmt, ForEachStmt, ForStmt, Stmt, WhileStmt};
use apex_syntax::{SyntaxKind, SyntaxNode};
use lsp_types::{
    CallHierarchyIncomingCall, CallHierarchyItem, CallHierarchyOutgoingCall, CodeAction,
    CodeActionKind, CodeActionOrCommand, CompletionItem, CompletionItemKind, CompletionList,
    CompletionResponse, CompletionTextEdit, Diagnostic, DiagnosticSeverity, DiagnosticTag,
    Documentation, DocumentHighlight, DocumentSymbol, FoldingRange, InlayHint, InlayHintKind,
    InlayHintLabel, InsertTextFormat, Location, ParameterInformation, ParameterLabel, Position,
    Range, SelectionRange, SignatureHelp, SignatureInformation, SymbolInformation,
    SymbolKind as LspSymbolKind, TextEdit, Url, WorkspaceEdit,
};
use rowan::ast::{support, AstNode};
use rowan::{TextRange, TextSize};
use std::collections::HashMap;

/// `uri`/`position` -> the `FileId`/byte-offset `program`'s own API
/// understands. `None` whenever `uri` doesn't name a file this bound
/// project actually knows about, or `position` falls outside it.
pub(crate) fn resolve_position(
    program: &BoundProgram,
    uri: &Url,
    position: Position,
    encoding: PositionEncoding,
) -> Option<(FileId, TextSize)> {
    let path = uri.to_file_path().ok()?;
    let file = program.file_id(&path)?;
    // The file's *bound* text, not `Backend::documents` -- guaranteed
    // consistent with whichever `program` snapshot is actually in hand,
    // even if a newer edit is mid-rebuild against a still-in-flight
    // `BindCache`.
    let text = program.syntax(file).text().to_string();
    let offset = LineIndex::new(&text).to_offset(&text, position, encoding)?;
    Some((file, offset.into()))
}

/// A resolved symbol's declaration site (its own name, not its whole
/// declaration -- matching `Symbol::name_range`'s own doc comment) as an
/// LSP `Location`.
pub(crate) fn symbol_location(
    program: &BoundProgram,
    id: SymbolId,
    encoding: PositionEncoding,
) -> Option<Location> {
    let symbol = program.symbols.get(id);
    let uri = Url::from_file_path(program.file_path(symbol.file)).ok()?;
    let text = program.syntax(symbol.file).text().to_string();
    let index = LineIndex::new(&text);
    let range = symbol.name_range;
    let start = index.to_position(&text, range.start().into(), encoding);
    let end = index.to_position(&text, range.end().into(), encoding);
    Some(Location {
        uri,
        range: Range { start, end },
    })
}

/// A reference site's own location -- the `references`/`document_highlights`
/// counterpart to `symbol_location`, which is declaration-site only
/// (keyed off a `Symbol`'s `name_range`). `ptr` here is a reference's own
/// `SyntaxPtr` (from `BoundProgram::references_to`/`references_to_in_file`),
/// so this keys off `ptr.file()` directly, and `BoundProgram::highlight_range(ptr)`
/// for the range rather than `ptr.range()` itself -- for a call/field-access
/// reference (`MethodCallExpr`, `CallExpr`, `FieldExpr`, a `NewExpr`
/// constructor call) `ptr.range()` spans the *whole* node (target through
/// closing paren for a call, receiver through member for a field access),
/// which would over-highlight the entire expression instead of just the
/// identifier the cursor is actually on.
pub(crate) fn ptr_location(
    program: &BoundProgram,
    ptr: SyntaxPtr,
    encoding: PositionEncoding,
) -> Option<Location> {
    let uri = Url::from_file_path(program.file_path(ptr.file())).ok()?;
    let text = program.syntax(ptr.file()).text().to_string();
    let index = LineIndex::new(&text);
    let range = program.highlight_range(ptr);
    let start = index.to_position(&text, range.start().into(), encoding);
    let end = index.to_position(&text, range.end().into(), encoding);
    Some(Location {
        uri,
        range: Range { start, end },
    })
}

/// A `Resolution::SchemaObject` resolution's goto-definition target: the
/// field's own `.field-meta.xml` when `r.field` names one, else the
/// object's own `.object-meta.xml`. `None` when there's no local file to
/// point at -- the object is only known here because a custom field was
/// added to a *standard* object (`Account` with a custom `Batch__c`
/// field, say), so `apex_metadata::SObjectSchema::object_path` is itself
/// `None` (no file locally describes `Account` the object). Always
/// points at the file's very start: `apex-metadata`'s XML parsing
/// doesn't track individual element positions, an honest v1 limit in
/// the same spirit as the still-open standard-schema gap this project
/// already documents (`BACKLOG.md` §4) -- landing on the right *file* is
/// still far more useful than no goto-definition support at all.
pub(crate) fn schema_location(program: &BoundProgram, r: &SchemaObjectRef) -> Option<Location> {
    let path: &std::path::Path = match &r.field {
        Some(field) => program.schema.field(&r.object, field)?.source_path.as_deref()?,
        None => program.schema.object(&r.object)?.object_path.as_deref()?,
    };
    let uri = Url::from_file_path(path).ok()?;
    Some(Location {
        uri,
        range: Range::default(),
    })
}

/// A `Resolution::Label` resolution's goto-definition target: the
/// `.labels-meta.xml` file the label was declared in. Always points at the
/// file's very start -- like `schema_location`, `apex-metadata`'s XML
/// parsing doesn't track individual `<labels>` element positions, so
/// landing on the right *file* (out of however many a project has) is the
/// achievable target here, not the exact `<labels>` block.
pub(crate) fn label_location(program: &BoundProgram, r: &LabelRef) -> Option<Location> {
    let label = program.labels.get(&r.full_name)?;
    let uri = Url::from_file_path(&label.source_path).ok()?;
    Some(Location {
        uri,
        range: Range::default(),
    })
}

/// Renders a `Resolution::Label` reference as Markdown hover text: the
/// label's own declared value (its Apex-visible display text), or just its
/// API name if the scraped `.labels-meta.xml` had no `<value>` -- the
/// custom-label equivalent of `describe_stdlib_member`, sourced from
/// project metadata (`program.labels`) instead of bundled stdlib data.
pub(crate) fn describe_label(program: &BoundProgram, r: &LabelRef) -> Option<String> {
    let label = program.labels.get(&r.full_name)?;
    Some(match &label.value {
        Some(value) => format!("```apex\nLabel.{}\n```\n\n{value}", label.full_name),
        None => format!("```apex\nLabel.{}\n```", label.full_name),
    })
}

/// A `Resolution::VisualforcePage` resolution's goto-definition target:
/// the page's own `.page` file. Always points at the file's very start --
/// there's no finer-grained position to point at (a `Page.<name>`
/// reference doesn't name any specific markup inside the page, just the
/// page as a whole).
pub(crate) fn visualforce_page_location(program: &BoundProgram, r: &VisualforcePageRef) -> Option<Location> {
    let page = program.pages.get(&r.name)?;
    let uri = Url::from_file_path(&page.path).ok()?;
    Some(Location {
        uri,
        range: Range::default(),
    })
}

/// Renders a `Resolution::VisualforcePage` reference as Markdown hover
/// text -- there's no further description to show beyond the page's own
/// name and that it's a `PageReference` (unlike a label, a `.page` file
/// has no single short "value" text of its own to surface), but a hover
/// still confirms the reference resolved to a real, specific page rather
/// than showing nothing at all.
pub(crate) fn describe_visualforce_page(program: &BoundProgram, r: &VisualforcePageRef) -> Option<String> {
    program
        .pages
        .get(&r.name)
        .map(|page| format!("```apex\nPage.{}\n```\n\nVisualforce page (`PageReference`)", page.name))
}

/// Renders `id` as Markdown hover text: a fenced-code signature line
/// (visibility/modifiers, kind-appropriate keyword, type, name, and --
/// for a method/constructor -- its parameter list via
/// `SymbolTable::params`), followed by its doc comment if it has one.
pub(crate) fn describe_symbol(program: &BoundProgram, id: SymbolId) -> String {
    let sig = symbol_signature(program, id);
    let mut out = format!("```apex\n{sig}\n```");
    if let Some(doc) = doc_comment_for(program, id) {
        if !doc.is_empty() {
            out.push_str("\n\n");
            out.push_str(&doc);
        }
    }
    out
}

/// The plain-text signature line `describe_symbol`'s Markdown wraps
/// (modifiers, kind-appropriate keyword, type, name, and -- for a
/// method/constructor -- its parameter list) -- factored out so
/// `capabilities::completion`'s `CompletionItem::detail` can reuse
/// exactly this formatting without a second implementation, since a
/// completion item's `detail` is a plain one-liner, not Markdown.
fn symbol_signature(program: &BoundProgram, id: SymbolId) -> String {
    let symbol = program.symbols.get(id);
    let mut sig = String::new();

    if matches!(
        symbol.kind,
        SymbolKind::Class
            | SymbolKind::Interface
            | SymbolKind::Enum
            | SymbolKind::Method
            | SymbolKind::Constructor
            | SymbolKind::Field
            | SymbolKind::Property
    ) {
        sig.push_str(match symbol.modifiers.visibility {
            Visibility::Public => "public ",
            Visibility::Private => "private ",
            Visibility::Protected => "protected ",
            Visibility::Global => "global ",
        });
        if symbol.modifiers.is_static {
            sig.push_str("static ");
        }
        if symbol.modifiers.is_abstract {
            sig.push_str("abstract ");
        }
        if symbol.modifiers.is_virtual {
            sig.push_str("virtual ");
        }
        if symbol.modifiers.is_override {
            sig.push_str("override ");
        }
        if symbol.modifiers.is_final {
            sig.push_str("final ");
        }
    }

    match symbol.kind {
        SymbolKind::Class => {
            sig.push_str("class ");
            sig.push_str(&symbol.name);
        }
        SymbolKind::Interface => {
            sig.push_str("interface ");
            sig.push_str(&symbol.name);
        }
        SymbolKind::Enum => {
            sig.push_str("enum ");
            sig.push_str(&symbol.name);
        }
        SymbolKind::Trigger => {
            sig.push_str("trigger ");
            sig.push_str(&symbol.name);
        }
        SymbolKind::Method => {
            sig.push_str(symbol.type_name.as_deref().unwrap_or("void"));
            sig.push(' ');
            sig.push_str(&symbol.name);
            sig.push('(');
            push_params(program, &mut sig, id);
            sig.push(')');
        }
        SymbolKind::Constructor => {
            sig.push_str(&symbol.name);
            sig.push('(');
            push_params(program, &mut sig, id);
            sig.push(')');
        }
        SymbolKind::EnumConstant => sig.push_str(&symbol.name),
        SymbolKind::Field
        | SymbolKind::Property
        | SymbolKind::Parameter
        | SymbolKind::LocalVar
        | SymbolKind::CatchVar
        | SymbolKind::ForEachVar
        | SymbolKind::SwitchBindingVar => {
            if let Some(t) = &symbol.type_name {
                sig.push_str(t);
                sig.push(' ');
            }
            sig.push_str(&symbol.name);
        }
    }

    sig
}

/// Renders a `Resolution::StdlibMember` reference as Markdown hover
/// text, the bundled-schema equivalent of `describe_symbol` -- there's
/// no `SymbolId`/declaration to render from, so this looks the class/
/// member back up through `program.stdlib` instead. `None` when
/// `program.stdlib` no longer has this exact class/member (shouldn't
/// happen for a resolution this same snapshot just produced, but the
/// index is looked up fresh rather than assumed).
///
/// A bare class reference (`r.member: None`, e.g. hovering `String` in
/// `String.isBlank(...)`) renders just the class name/namespace -- there's
/// no per-class description in the bundled snapshot to show beyond that,
/// unlike a method/property. A method call narrows to `r.narrowed_param_types`'s
/// one exact overload when bind time already found one (real argument
/// *type*, not just arity -- see `crate::resolve::narrow_stdlib_overload`'s
/// own doc comment for why this needs bind time's real `Ty`s and can't
/// just be redone here from `r` alone), falling back to arity-only
/// narrowing via `r.arg_count` otherwise -- and to showing every overload
/// only when neither narrows to at least one (an unknown arg count, or
/// the arity-matching set is somehow empty). Each surviving overload gets
/// its own signature line, followed by the first one's description (real
/// overloads of the same method overwhelmingly share one description in
/// the scraped docs).
pub(crate) fn describe_stdlib_member(program: &BoundProgram, r: &StdlibMemberRef) -> Option<String> {
    let class = program.stdlib.class(&r.class_name)?;
    let ns_prefix = class
        .namespace
        .as_deref()
        .map(|ns| format!("{ns}."))
        .unwrap_or_default();

    let Some(member) = &r.member else {
        return Some(format!("```apex\n{ns_prefix}{}\n```", class.name));
    };

    if let Some(prop) = program.stdlib.property(&r.class_name, member) {
        let modifier = if prop.is_static { "static " } else { "" };
        let ty = prop.type_name.as_deref().unwrap_or("Object");
        let mut out = format!("```apex\npublic {modifier}{ty} {ns_prefix}{}\n```", prop.name);
        if let Some(desc) = &prop.description {
            out.push_str("\n\n");
            out.push_str(desc);
        }
        return Some(out);
    }

    let overloads: Vec<_> = program.stdlib.methods(&r.class_name, member).collect();
    // `narrowed_param_types`, when present, already names the *one* real
    // overload `crate::resolve::narrow_stdlib_overload` picked by
    // argument type, not just arity -- a same-arity overload pair (`List.addAll(List)`
    // vs. `addAll(Set)`, both one parameter, but List/Set aren't
    // implicitly convertible) would otherwise show both signatures on
    // hover with no way to tell which one a real call actually reaches.
    // Falls back to the old arity-only filter whenever it's absent (a
    // single overload to begin with, genuine ambiguity, or an
    // incompletely-scraped winner).
    let narrowed: Vec<_> = match &r.narrowed_param_types {
        Some(param_types) => overloads
            .iter()
            .copied()
            .filter(|m| {
                m.params.len() == param_types.len()
                    && m.params
                        .iter()
                        .zip(param_types.iter())
                        .all(|(p, t)| p.type_name.as_deref() == Some(t.as_str()))
            })
            .collect(),
        None => match r.arg_count {
            Some(n) => overloads.iter().copied().filter(|m| m.params.len() == n).collect(),
            None => Vec::new(),
        },
    };
    let overloads = if narrowed.is_empty() { &overloads } else { &narrowed };
    let (first, rest) = overloads.split_first()?;
    let mut sig = String::from("```apex\n");
    for m in std::iter::once(first).chain(rest.iter()) {
        let modifier = if m.is_static { "static " } else { "" };
        let ret = m.return_type.as_deref().unwrap_or("void");
        let params = m
            .params
            .iter()
            .map(stdlib_param_label)
            .collect::<Vec<_>>()
            .join(", ");
        sig.push_str(&format!("public {modifier}{ret} {ns_prefix}{}({params})\n", m.name));
    }
    sig.push_str("```");
    if let Some(desc) = &first.description {
        sig.push_str("\n\n");
        sig.push_str(desc);
    }
    Some(sig)
}

fn push_params(program: &BoundProgram, out: &mut String, method_or_ctor: SymbolId) {
    let params = program.symbols.params(method_or_ctor);
    for (i, param_id) in params.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        let param = program.symbols.get(*param_id);
        if let Some(t) = &param.type_name {
            out.push_str(t);
            out.push(' ');
        }
        out.push_str(&param.name);
    }
}

/// A scraped stdlib parameter's own `"Type name"` label, mirroring
/// `push_params`'s declared-parameter formatting -- falls back to just
/// the type (or `"Object"` if even that's missing) when the scraper
/// couldn't extract a name for this particular parameter (see
/// `apex_stdlib::StdlibParam`'s own doc comment for why that happens).
fn stdlib_param_label(p: &apex_stdlib::StdlibParam) -> String {
    let ty = p.type_name.as_deref().unwrap_or("Object");
    match &p.name {
        Some(name) => format!("{ty} {name}"),
        None => ty.to_string(),
    }
}

/// `id`'s doc comment, if its declaration has one. Every
/// `HasDocComment`-implementing declaration node (`ClassDecl`/
/// `InterfaceDecl`/`EnumDecl`/`MethodDecl`/`ConstructorDecl`/
/// `PropertyDecl`/`TriggerUnit`) is exactly what `symbol.ptr` already
/// points at -- except `Field`, whose `ptr` points at its own
/// `VarDeclarator` (a field statement can declare more than one name:
/// `Integer a, b;`), one level below the `FieldDecl` a doc comment
/// actually attaches to, so that one case climbs to the parent first.
/// A symbol kind that belongs in an outline/search result -- excludes
/// parameters and every local-variable kind, which don't belong in a
/// document outline or a project-wide symbol search the way a type or
/// member does. Shared by [`document_symbols`] and [`workspace_symbols`].
fn is_outline_kind(kind: SymbolKind) -> bool {
    !matches!(
        kind,
        SymbolKind::Parameter
            | SymbolKind::LocalVar
            | SymbolKind::CatchVar
            | SymbolKind::ForEachVar
            | SymbolKind::SwitchBindingVar
    )
}

/// `apex_binder::SymbolKind` -> the closest `lsp_types::SymbolKind`.
/// `Trigger` has no direct LSP equivalent -- `EVENT` is the closest
/// semantic fit (a trigger responds to a DDL event), not a literal
/// mapping. The local-variable kinds are never actually reached (see
/// [`is_outline_kind`]) but are mapped anyway to keep this exhaustive.
fn lsp_symbol_kind(kind: SymbolKind) -> LspSymbolKind {
    match kind {
        SymbolKind::Class => LspSymbolKind::CLASS,
        SymbolKind::Interface => LspSymbolKind::INTERFACE,
        SymbolKind::Enum => LspSymbolKind::ENUM,
        SymbolKind::EnumConstant => LspSymbolKind::ENUM_MEMBER,
        SymbolKind::Trigger => LspSymbolKind::EVENT,
        SymbolKind::Method => LspSymbolKind::METHOD,
        SymbolKind::Constructor => LspSymbolKind::CONSTRUCTOR,
        SymbolKind::Field => LspSymbolKind::FIELD,
        SymbolKind::Property => LspSymbolKind::PROPERTY,
        SymbolKind::Parameter
        | SymbolKind::LocalVar
        | SymbolKind::CatchVar
        | SymbolKind::ForEachVar
        | SymbolKind::SwitchBindingVar => LspSymbolKind::VARIABLE,
    }
}

/// `textDocument/documentSymbol`'s outline tree for `file`: every
/// declaration-shaped symbol (`is_outline_kind`), nested by
/// `Symbol::container` -- a type's members and any nested types become
/// its `children`, top-level types (`container: None`) are the roots.
pub(crate) fn document_symbols(
    program: &BoundProgram,
    file: FileId,
    encoding: PositionEncoding,
) -> Vec<DocumentSymbol> {
    let root = program.syntax(file);
    let text = root.text().to_string();
    let index = LineIndex::new(&text);
    let to_range = |r: TextRange| Range {
        start: index.to_position(&text, r.start().into(), encoding),
        end: index.to_position(&text, r.end().into(), encoding),
    };

    let entries: Vec<(SymbolId, &Symbol)> = program
        .symbols
        .iter()
        .filter(|(_, s)| s.file == file && is_outline_kind(s.kind))
        .collect();

    fn build(
        entries: &[(SymbolId, &Symbol)],
        container: Option<SymbolId>,
        root: &apex_syntax::SyntaxNode,
        to_range: &impl Fn(TextRange) -> Range,
    ) -> Vec<DocumentSymbol> {
        entries
            .iter()
            .filter(|entry| entry.1.container == container)
            .map(|&(id, s)| {
                let children = build(entries, Some(id), root, to_range);
                #[allow(deprecated)]
                // `deprecated` field, superseded by `tags` -- neither used here
                DocumentSymbol {
                    name: s.name.to_string(),
                    detail: s.type_name.as_ref().map(|t| t.to_string()),
                    kind: lsp_symbol_kind(s.kind),
                    tags: None,
                    deprecated: None,
                    range: to_range(declaration_range(root, s.ptr)),
                    selection_range: to_range(s.name_range),
                    children: (!children.is_empty()).then_some(children),
                }
            })
            .collect()
    }
    build(&entries, None, &root, &to_range)
}

/// `ptr`'s own range, but starting at its first non-trivia token instead
/// of wherever its span literally begins -- a declaration's doc comment
/// ends up nested *inside* its own node (see `apex_syntax::ast::decl::HasDocComment`'s
/// doc comment: it attaches as leading trivia on the first real child,
/// e.g. the first `Modifier`, not as a sibling before the node), so the
/// node's raw `text_range()` starts at the doc comment, not the
/// declaration. Left as the raw range if `ptr` fails to resolve
/// (defensive; shouldn't happen against this file's own root).
fn declaration_range(root: &apex_syntax::SyntaxNode, ptr: apex_binder::SyntaxPtr) -> TextRange {
    let raw = ptr.range();
    let Some(node) = ptr.to_node(root) else {
        return raw;
    };
    let start = node
        .descendants_with_tokens()
        .find_map(|elem| {
            let t = elem.as_token()?;
            (!t.kind().is_trivia()).then(|| t.text_range().start())
        })
        .unwrap_or_else(|| raw.start());
    TextRange::new(start, raw.end())
}

/// `workspace/symbol`'s project-wide search: every declaration-shaped
/// symbol (`is_outline_kind`) whose name contains `query`,
/// case-insensitively. A plain substring scan, not fuzzy scoring --
/// simplest correct baseline, matching how many language servers start
/// before layering ranking on top.
pub(crate) fn workspace_symbols(
    program: &BoundProgram,
    query: &str,
    encoding: PositionEncoding,
) -> Vec<SymbolInformation> {
    let query = query.to_lowercase();
    program
        .symbols
        .iter()
        .filter(|(_, s)| is_outline_kind(s.kind) && s.name.to_lowercase().contains(&query))
        .filter_map(|(id, s)| {
            let location = symbol_location(program, id, encoding)?;
            let container_name = s.container.map(|c| program.symbols.get(c).name.to_string());
            #[allow(deprecated)] // `deprecated` field, superseded by `tags` -- neither used here
            Some(SymbolInformation {
                name: s.name.to_string(),
                kind: lsp_symbol_kind(s.kind),
                tags: None,
                deprecated: None,
                location,
                container_name,
            })
        })
        .collect()
}

/// The `SymbolId`(s) a cursor position names -- shared by `references`
/// and `document_highlights`. Mirrors `hover`'s own precedence
/// (`main.rs`): a declaration's own name (`symbol_at`) first, else a
/// reference's resolution (`resolution_at`)'s `Resolved`/`Candidates` --
/// `Candidates` contributes every candidate rather than silently picking
/// one, the same "don't guess" convention `hover`/`definition` already
/// use for an ambiguous overload.
fn targets_at(program: &BoundProgram, file: FileId, offset: TextSize) -> Vec<SymbolId> {
    if let Some(id) = program.symbol_at(file, offset) {
        return vec![id];
    }
    match program.resolution_at(file, offset) {
        Some(Resolution::Resolved(id)) => vec![*id],
        Some(Resolution::Candidates(ids)) => ids.clone(),
        _ => Vec::new(),
    }
}

/// `textDocument/references`: every location (project-wide) referencing
/// the symbol at `file`/`offset`, via `BoundProgram::references_to`'s
/// reverse-index lookup (`BACKLOG.md` §3) -- not a scan. `include_declaration`
/// additionally prepends each target's own declaration site.
///
/// Also handles a cursor on a `SchemaObject`/`UnknownSchema`/`StdlibMember`
/// reference (a real Salesforce field or stdlib class/member, neither of
/// which has a `SymbolId` for `targets_at`/`references_to` to key on) via
/// `Resolution::external_key`/`BoundProgram::references_to_external`'s
/// parallel reverse index -- see that type's own doc comment for why
/// this needs a second index rather than reusing `by_symbol`.
/// `include_declaration` only ever adds something for `SchemaObject`
/// (`schema_location`'s `.object-meta.xml` target) -- `UnknownSchema`/
/// `StdlibMember` are both, by construction, real things with no local
/// declaration site at all (see `Resolution`'s own doc comment).
pub(crate) fn references(
    program: &BoundProgram,
    file: FileId,
    offset: TextSize,
    include_declaration: bool,
    encoding: PositionEncoding,
) -> Vec<Location> {
    let targets = targets_at(program, file, offset);
    let mut locations = Vec::new();
    if include_declaration {
        locations.extend(
            targets
                .iter()
                .filter_map(|&id| symbol_location(program, id, encoding)),
        );
    }
    locations.extend(targets.iter().flat_map(|&id| {
        program
            .references_to(id)
            .filter_map(|ptr| ptr_location(program, ptr, encoding))
    }));

    if let Some(resolution) = program.resolution_at(file, offset) {
        if let Some(key) = resolution.external_key() {
            if include_declaration {
                if let Resolution::SchemaObject(r) = resolution {
                    locations.extend(schema_location(program, r));
                }
            }
            locations.extend(
                program
                    .references_to_external(&key)
                    .filter_map(|ptr| ptr_location(program, ptr, encoding)),
            );
        }
    }

    locations
}

/// `textDocument/documentHighlight`: every occurrence of the symbol at
/// `file`/`offset`, scoped to `file` alone -- `BoundProgram::references_to_in_file`,
/// the cheaper per-file counterpart to `references_to` `references` above
/// uses project-wide. Always includes the declaration site when it falls
/// in `file` (matching typical editor "highlight all occurrences in this
/// file" UX, unlike `references`, which only includes it when asked).
/// No `DocumentHighlightKind` (`Read`/`Write`) distinction -- this binder
/// has no assignment-target tracking to base one on, so every highlight
/// stays the LSP-default `Text` kind (`kind: None`) rather than inventing
/// a distinction the resolver doesn't actually make.
///
/// Also covers a `SchemaObject`/`UnknownSchema`/`StdlibMember` reference
/// via `BoundProgram::references_to_external_in_file` -- see `references`'s
/// own doc comment above for why that needs a separate, non-`SymbolId`
/// lookup.
pub(crate) fn document_highlights(
    program: &BoundProgram,
    file: FileId,
    offset: TextSize,
    encoding: PositionEncoding,
) -> Vec<DocumentHighlight> {
    let targets = targets_at(program, file, offset);
    let mut ranges: Vec<Range> = Vec::new();
    for &id in &targets {
        let symbol = program.symbols.get(id);
        if symbol.file == file {
            if let Some(loc) = symbol_location(program, id, encoding) {
                ranges.push(loc.range);
            }
        }
    }
    for &id in &targets {
        for ptr in program.references_to_in_file(file, id) {
            if let Some(loc) = ptr_location(program, ptr, encoding) {
                ranges.push(loc.range);
            }
        }
    }

    if let Some(key) = program
        .resolution_at(file, offset)
        .and_then(Resolution::external_key)
    {
        for ptr in program.references_to_external_in_file(file, &key) {
            if let Some(loc) = ptr_location(program, ptr, encoding) {
                ranges.push(loc.range);
            }
        }
    }

    ranges
        .into_iter()
        .map(|range| DocumentHighlight { range, kind: None })
        .collect()
}

/// `textDocument/foldingRange`: every brace-delimited region (a class/
/// interface body, a method/constructor/property-accessor/trigger body,
/// a `{ ... }` collection initializer) that spans more than one line.
/// Directly derivable from the CST alone -- no semantic/binder info
/// needed, so this never fails or comes back empty just because the
/// bind hasn't caught up with the very latest edit. Deliberately line-
/// granular (no `start_character`/`end_character`): the brace's own
/// line stays visible when collapsed (`{...}`), matching how most
/// editors fold anyway.
pub(crate) fn folding_ranges(program: &BoundProgram, file: FileId) -> Vec<FoldingRange> {
    const FOLDABLE_KINDS: [apex_syntax::SyntaxKind; 7] = [
        apex_syntax::SyntaxKind::ClassBody,
        apex_syntax::SyntaxKind::InterfaceBody,
        apex_syntax::SyntaxKind::Block,
        apex_syntax::SyntaxKind::TriggerBlock,
        apex_syntax::SyntaxKind::ArrayInitializer,
        apex_syntax::SyntaxKind::SetInitializer,
        apex_syntax::SyntaxKind::MapInitializer,
    ];
    let text = program.syntax(file).text().to_string();
    let index = LineIndex::new(&text);
    // The encoding passed here only ever affects `Position::character`,
    // which a line-granular folding range never reports -- `Utf8` is an
    // arbitrary, cost-free choice, not a real encoding decision.
    let line_of = |offset: TextSize| {
        index
            .to_position(&text, offset.into(), PositionEncoding::Utf8)
            .line
    };

    program
        .syntax(file)
        .descendants()
        .filter(|n| FOLDABLE_KINDS.contains(&n.kind()))
        .filter_map(|n| {
            let range = n.text_range();
            let start_line = line_of(range.start());
            let end_line = line_of(range.end());
            (start_line != end_line).then_some(FoldingRange {
                start_line,
                start_character: None,
                end_line,
                end_character: None,
                kind: None,
                collapsed_text: None,
            })
        })
        .collect()
}

/// `textDocument/selectionRange` for one position: every ancestor
/// node's range, innermost first, chained via `parent` -- rowan's tree
/// already *is* exactly this nesting, so no semantic/binder info is
/// needed, only the CST. Consecutive levels with an identical range
/// (a single-child wrapper node) collapse into one, so an "expand
/// selection" command never appears to do nothing.
pub(crate) fn selection_range_at(
    program: &BoundProgram,
    file: FileId,
    offset: TextSize,
    encoding: PositionEncoding,
) -> Option<SelectionRange> {
    let text = program.syntax(file).text().to_string();
    let index = LineIndex::new(&text);
    let to_range = |r: TextRange| Range {
        start: index.to_position(&text, r.start().into(), encoding),
        end: index.to_position(&text, r.end().into(), encoding),
    };

    let root = program.syntax(file);
    let token = match root.token_at_offset(offset) {
        rowan::TokenAtOffset::None => return None,
        rowan::TokenAtOffset::Single(t) => t,
        rowan::TokenAtOffset::Between(left, right) => {
            if left.kind().is_trivia() {
                right
            } else {
                left
            }
        }
    };

    let mut chain: Vec<TextRange> = vec![token.text_range()];
    let mut node = token.parent();
    while let Some(n) = node {
        let r = n.text_range();
        if chain.last() != Some(&r) {
            chain.push(r);
        }
        node = n.parent();
    }

    let mut result: Option<SelectionRange> = None;
    for range in chain.into_iter().rev() {
        result = Some(SelectionRange {
            range: to_range(range),
            parent: result.map(Box::new),
        });
    }
    result
}

/// Why `textDocument/rename`/`prepareRename` refused a target -- every
/// variant becomes a `ResponseError` message in `main.rs`, never a
/// silently empty or partial result. `references`/`document_highlight`
/// can afford to show every `Resolution::Candidates` entry and let the
/// user look; a rename is a mutating, project-wide operation, so the same
/// "don't guess" convention means refusing outright here instead.
pub(crate) enum RenameRefusal {
    NoSymbolHere,
    Ambiguous,
    Trigger,
    ImplicitValue,
    OverrideChain(&'static str),
    InvalidIdentifier,
    NameCollision,
}

impl RenameRefusal {
    pub(crate) fn message(&self) -> String {
        match self {
            RenameRefusal::NoSymbolHere => "no renameable symbol at this position".to_string(),
            RenameRefusal::Ambiguous => "this reference is ambiguous (an unresolved overload or \
                 unmodeled type) -- renaming could silently rewrite the wrong call site"
                .to_string(),
            RenameRefusal::Trigger => "a trigger's name is tied to its Salesforce object, not a \
                 renameable identifier"
                .to_string(),
            RenameRefusal::ImplicitValue => "`value` is a property setter's implicit parameter -- \
                 it has no declaration in source to rename"
                .to_string(),
            RenameRefusal::OverrideChain(reason) => {
                format!("can't safely rename this method: {reason}")
            }
            RenameRefusal::InvalidIdentifier => {
                "the new name isn't a valid Apex identifier".to_string()
            }
            RenameRefusal::NameCollision => {
                "the new name would collide with an existing declaration".to_string()
            }
        }
    }
}

/// The single, safely-renameable `SymbolId` at `file`/`offset`, or why
/// not. Mirrors `targets_at`'s declaration-then-reference precedence, but
/// -- unlike the read-only requests that reuse `targets_at` and can afford
/// to show every `Candidates` entry -- a rename must resolve to *exactly*
/// one target and every one of its references must resolve just as
/// cleanly, or it refuses outright.
pub(crate) fn rename_target(
    program: &BoundProgram,
    file: FileId,
    offset: TextSize,
) -> Result<SymbolId, RenameRefusal> {
    let id = if let Some(id) = program.symbol_at(file, offset) {
        id
    } else {
        match program.resolution_at(file, offset) {
            Some(Resolution::Resolved(id)) => *id,
            Some(Resolution::Candidates(_)) => return Err(RenameRefusal::Ambiguous),
            _ => return Err(RenameRefusal::NoSymbolHere),
        }
    };

    let symbol = program.symbols.get(id);
    if symbol.kind == SymbolKind::Trigger {
        return Err(RenameRefusal::Trigger);
    }
    // A property setter's implicit `value` parameter (`crate::collect`'s
    // `collect_property`) is a real `Parameter` symbol so ordinary
    // resolution/hover treat it like any other local, but it's kept under
    // the *property's* own id as `container` (never a class's) specifically
    // so it stays invisible everywhere else -- including here, where its
    // `name_range` points at the `get`/`set` keyword token (there's no real
    // `value` token in source to point at instead), which a rename would
    // otherwise silently overwrite.
    if symbol.kind == SymbolKind::Parameter
        && symbol
            .container
            .is_some_and(|c| program.symbols.get(c).kind == SymbolKind::Property)
    {
        return Err(RenameRefusal::ImplicitValue);
    }
    if symbol.kind == SymbolKind::Method {
        check_method_eligible(program, id)?;
    }

    if !references_resolve_cleanly(program, id) {
        return Err(RenameRefusal::Ambiguous);
    }
    Ok(id)
}

/// Whether every reference `program.references_to(id)` reports actually
/// resolves *exactly* to `id` alone (`Resolution::Resolved`), not as one
/// entry of an ambiguous `Resolution::Candidates` set it also happens to
/// belong to (`ReferenceTable`'s reverse index deliberately includes a
/// reference in *every* candidate's `by_symbol` entry, not just a
/// silently-picked one -- see `BACKLOG.md` §3 -- so `references_to`
/// alone can't tell the two cases apart).
fn references_resolve_cleanly(program: &BoundProgram, id: SymbolId) -> bool {
    program.references_to(id).all(|ptr| {
        matches!(program.resolution(ptr), Some(Resolution::Resolved(resolved)) if *resolved == id)
    })
}

/// A `Method` can safely be treated as having exactly one, non-cascading
/// declaration only when it can't be part of an override chain (a distinct,
/// unsolved problem from overload-call ambiguity): not itself marked
/// `override`, not implementing an interface/base-class method of the same
/// name and arity (Apex requires no `override` keyword for that case), and
/// not itself overridden by any subclass. `None` when clear; `Some(reason)`
/// otherwise, for a caller to fold into its own refusal type (`check_method_eligible`
/// for rename's `RenameRefusal`, `crate::capabilities::parameter_reorder_actions`
/// for its own quiet "offer nothing" gate). The last check scans every
/// project symbol -- no index answers "which methods override this one" the
/// way `SymbolTable`'s other lookups are O(1), but that's fine here: both
/// callers are rare, user-initiated actions, not a per-keystroke path the
/// rest of this codebase optimizes for.
fn method_override_chain_reason(program: &BoundProgram, id: SymbolId) -> Option<&'static str> {
    let symbol = program.symbols.get(id);
    if symbol.modifiers.is_override {
        return Some("it overrides a base class method");
    }
    let container = symbol.container?;
    let arity = program.symbols.params(id).len();

    for &ancestor in program.symbols.inherited_chain(container) {
        for candidate in program.symbols.lookup_member(ancestor, &symbol.name) {
            if candidate != id && program.symbols.params(candidate).len() == arity {
                return Some("it implements an interface or base-class method of the same name");
            }
        }
    }

    let overridden_by_subclass = program.symbols.iter().any(|(other_id, other)| {
        other_id != id
            && other.kind == SymbolKind::Method
            && other.modifiers.is_override
            && other.name.eq_ignore_ascii_case(&symbol.name)
            && program.symbols.params(other_id).len() == arity
            && other
                .container
                .is_some_and(|c| program.symbols.inherited_chain(c).contains(&container))
    });
    if overridden_by_subclass {
        return Some("it's overridden by a subclass");
    }
    None
}

/// A `Method` is renameable only when [`method_override_chain_reason`]
/// finds no reason it can't be -- see that function's own doc comment for
/// what each case means.
fn check_method_eligible(program: &BoundProgram, id: SymbolId) -> Result<(), RenameRefusal> {
    match method_override_chain_reason(program, id) {
        Some(reason) => Err(RenameRefusal::OverrideChain(reason)),
        None => Ok(()),
    }
}

/// Whether `new_name` is a syntactically legal, non-keyword Apex
/// identifier -- reuses `apex_lexer`'s own tokenizer rather than hand-
/// rolling a second identifier/keyword rule set: `new_name` is legal only
/// when it lexes as *exactly one* `Identifier` token (a keyword lexes as
/// its own distinct `TokenKind`, and anything containing whitespace/
/// punctuation/more than one word lexes as more than one token).
fn is_valid_new_identifier(new_name: &str) -> bool {
    let tokens = apex_lexer::tokenize(new_name);
    tokens.len() == 1 && tokens[0].kind == apex_lexer::TokenKind::Identifier
}

/// Whether renaming `id` to `new_name` would collide with an existing
/// declaration -- cheap, using only indices `SymbolTable` already builds
/// (`top_level`/`lookup_member`, both already O(1)/case-insensitive).
/// Honest v1 gap: a `Parameter`/local-variable kind isn't checked here --
/// that needs a scope-tree walk this pass doesn't attempt yet, a narrower
/// gap than skipping collision detection entirely (every other kind is
/// still fully covered), and a real compiler still catches an actual
/// shadowing conflict immediately, unlike a member/type collision, which
/// can silently shadow across the whole project.
fn renamed_symbol_collides(program: &BoundProgram, id: SymbolId, new_name: &str) -> bool {
    let symbol = program.symbols.get(id);
    match symbol.kind {
        SymbolKind::Class | SymbolKind::Interface | SymbolKind::Enum => {
            program.symbols.top_level(new_name).is_some()
        }
        SymbolKind::Field
        | SymbolKind::Property
        | SymbolKind::Method
        | SymbolKind::Constructor
        | SymbolKind::EnumConstant => symbol.container.is_some_and(|container| {
            program
                .symbols
                .lookup_member(container, new_name)
                .into_iter()
                .any(|other| other != id)
        }),
        _ => false,
    }
}

/// `textDocument/prepareRename`'s range: the plain identifier token under
/// the cursor, independent of whether it's a declaration or a reference --
/// once `rename_target` has already confirmed the position names exactly
/// one safely-renameable symbol, the client just needs the span its rename
/// input box should cover. Same token-lookup pattern as `selection_range_at`.
pub(crate) fn prepare_rename_range(
    program: &BoundProgram,
    file: FileId,
    offset: TextSize,
    encoding: PositionEncoding,
) -> Option<Range> {
    let root = program.syntax(file);
    let token = match root.token_at_offset(offset) {
        rowan::TokenAtOffset::None => return None,
        rowan::TokenAtOffset::Single(t) => t,
        rowan::TokenAtOffset::Between(left, right) => {
            if left.kind().is_trivia() {
                right
            } else {
                left
            }
        }
    };
    let text = root.text().to_string();
    let index = LineIndex::new(&text);
    let range = token.text_range();
    Some(Range {
        start: index.to_position(&text, range.start().into(), encoding),
        end: index.to_position(&text, range.end().into(), encoding),
    })
}

/// `textDocument/rename`: a project-wide `WorkspaceEdit` renaming the
/// symbol at `file`/`offset` to `new_name`, or why not (`RenameRefusal`).
/// Reuses exactly the data `references`/`document_highlight` already
/// gather -- `references_to`'s reverse-index lookup, `highlight_range`'s
/// narrow identifier-only range -- so this is `O(references)`, not a
/// project-wide scan; a `LineIndex` is built at most once per touched
/// file, not once per project file.
pub(crate) fn rename_edits(
    program: &BoundProgram,
    file: FileId,
    offset: TextSize,
    new_name: &str,
    encoding: PositionEncoding,
) -> Result<WorkspaceEdit, RenameRefusal> {
    if !is_valid_new_identifier(new_name) {
        return Err(RenameRefusal::InvalidIdentifier);
    }
    let id = rename_target(program, file, offset)?;
    if renamed_symbol_collides(program, id, new_name) {
        return Err(RenameRefusal::NameCollision);
    }

    // Every edit site: the declaration itself, a class rename's own
    // constructors (Apex requires a constructor's name to exactly match
    // its class's -- a separate `Symbol` with its own `name_range`, never
    // itself recorded as a *reference* to the class), and every project-
    // wide reference.
    let symbol = program.symbols.get(id);
    let mut sites: Vec<(FileId, TextRange)> = vec![(symbol.file, symbol.name_range)];
    if symbol.kind == SymbolKind::Class {
        sites.extend(
            program
                .symbols
                .members_of(id)
                .iter()
                .map(|&m| program.symbols.get(m))
                .filter(|m| m.kind == SymbolKind::Constructor)
                .map(|m| (m.file, m.name_range)),
        );
    }
    sites.extend(
        program
            .references_to(id)
            .map(|ptr| (ptr.file(), program.highlight_range(ptr))),
    );

    let mut per_file: HashMap<FileId, (Url, String, LineIndex)> = HashMap::new();
    let mut changes: HashMap<Url, Vec<TextEdit>> = HashMap::new();
    for (site_file, range) in sites {
        if let std::collections::hash_map::Entry::Vacant(e) = per_file.entry(site_file) {
            let Some(uri) = Url::from_file_path(program.file_path(site_file)).ok() else {
                continue;
            };
            let text = program.syntax(site_file).text().to_string();
            let index = LineIndex::new(&text);
            e.insert((uri, text, index));
        }
        let Some((uri, text, index)) = per_file.get(&site_file) else {
            continue;
        };
        let start = index.to_position(text, range.start().into(), encoding);
        let end = index.to_position(text, range.end().into(), encoding);
        changes.entry(uri.clone()).or_default().push(TextEdit {
            range: Range { start, end },
            new_text: new_name.to_string(),
        });
    }

    Ok(WorkspaceEdit {
        changes: Some(changes),
        ..Default::default()
    })
}

fn doc_comment_for(program: &BoundProgram, id: SymbolId) -> Option<String> {
    let symbol = program.symbols.get(id);
    let root = program.syntax(symbol.file);
    let node = symbol.ptr.to_node(&root)?;
    match symbol.kind {
        SymbolKind::Class => ClassDecl::cast(node)?.doc_comment_text(),
        SymbolKind::Interface => InterfaceDecl::cast(node)?.doc_comment_text(),
        SymbolKind::Enum => EnumDecl::cast(node)?.doc_comment_text(),
        SymbolKind::Method => MethodDecl::cast(node)?.doc_comment_text(),
        SymbolKind::Constructor => ConstructorDecl::cast(node)?.doc_comment_text(),
        SymbolKind::Property => PropertyDecl::cast(node)?.doc_comment_text(),
        SymbolKind::Trigger => TriggerUnit::cast(node)?.doc_comment_text(),
        SymbolKind::Field => FieldDecl::cast(node.parent()?)?.doc_comment_text(),
        _ => None,
    }
}

/// `textDocument/publishDiagnostics`: one `ERROR`-severity diagnostic per
/// `apex_parser::ParseError` recorded for `file` (`BoundProgram::syntax_errors`)
/// -- the parser's own "never panics, always records an error plus a
/// best-effort tree" guarantee means this is just surfacing data that
/// already existed, not computing anything new. `ParseError` only carries
/// a single byte offset, not a span, so the range is deliberately just the
/// one character at that offset (zero-width, clamped to the file's own
/// length, for an error recorded at end-of-file, e.g. "expected Semi,
/// found Eof") -- an honest v1 choice, not an oversight: extending to the
/// nearest real token's full span would need either sniffing the message
/// string for "found X" (fragile) or a token-at-offset lookup with its own
/// edge cases (the offset landing in trivia, or genuinely at EOF with
/// nothing to extend to). The message itself is passed through verbatim --
/// the parser's own `"expected RParen, found Dot"`-style text already
/// states what was expected and what was found, without a separate
/// humanization layer translating token names to their real spelling.
/// Every diagnostic source for one file, merged in the same order
/// `crate::publish_diagnostics` (push) and `Backend::document_diagnostic`
/// (pull) both need -- the two transports differ only in *when* they call
/// this and how they wrap the result, never in what diagnostics a file gets.
pub(crate) fn diagnostics_for_file(
    program: &BoundProgram,
    file: FileId,
    encoding: PositionEncoding,
) -> Vec<Diagnostic> {
    let mut diagnostics = syntax_error_diagnostics(program, file, encoding);
    diagnostics.extend(dead_code_diagnostics(program, file, encoding));
    diagnostics.extend(unresolved_reference_diagnostics(program, file, encoding));
    diagnostics.extend(unknown_schema_diagnostics(program, file, encoding));
    diagnostics.extend(modifier_diagnostics(program, file, encoding));
    diagnostics.extend(bulkification_diagnostics(program, file, encoding));
    diagnostics.extend(unreachable_code_diagnostics(program, file, encoding));
    diagnostics.extend(missing_implementation_diagnostics(program, file, encoding));
    diagnostics.extend(type_mismatch_diagnostics(program, file, encoding));
    diagnostics.extend(visibility_narrowing_diagnostics(program, file, encoding));
    diagnostics
}

pub(crate) fn syntax_error_diagnostics(
    program: &BoundProgram,
    file: FileId,
    encoding: PositionEncoding,
) -> Vec<Diagnostic> {
    let text = program.syntax(file).text().to_string();
    let index = LineIndex::new(&text);
    let len = text.len() as u32;
    program
        .syntax_errors(file)
        .iter()
        .map(|err| {
            let start = err.offset.min(len);
            let end = (err.offset + 1).min(len);
            Diagnostic {
                range: Range {
                    start: index.to_position(&text, start, encoding),
                    end: index.to_position(&text, end, encoding),
                },
                severity: Some(DiagnosticSeverity::ERROR),
                source: Some("apexls".to_string()),
                message: err.message.clone(),
                ..Default::default()
            }
        })
        .collect()
}

/// `Some(reason)` when `ptr` matches one of the specific, identified
/// shapes where the binder produces `Resolution::Unresolved` not because
/// the referenced name doesn't exist in real Apex, but because this
/// binder's own resolution has no fallback for that shape at all --
/// catch-clause/`whenValue`/`upsert`-external-id lookups
/// (`SymbolTable::resolve_dotted_name`, project-local only, no stdlib or
/// schema fallback), one segment of a namespace-qualified stdlib type
/// (`resolve::record_qualified_segments`, the one case a bare *token*
/// gets its own recorded `Resolution` -- see `SyntaxPtr::for_token`'s doc
/// comment), a bare `super` reference or `super(...)`/`this(...)` call
/// when the enclosing type's own supertype couldn't be resolved
/// (`SymbolTable::direct_super`), or a SOQL field reference with no
/// single object to resolve against (most commonly a `TYPEOF ... ELSE`
/// field, `soql::bind_typeof`'s own doc comment: "always `Unresolved`").
/// `None` for everything else (`NameExpr`, `FieldExpr`, `MethodCallExpr`,
/// `NewExpr`, `Type` not shaped like a built-in exception subtype's name,
/// `ThisExpr`, an unqualified non-`this`/`super` call) -- the higher-
/// confidence default, on the theory that a reference this
/// investigation couldn't specifically explain away is more likely a real
/// typo than not. This is a best-effort heuristic, not a proof: the
/// `CallExpr` case in particular can't distinguish "the supertype itself
/// is unresolvable" from "the supertype resolved fine but no constructor
/// overload matched these arguments" (both funnel into the same
/// `Resolution::Unresolved` at the same `ptr` -- see
/// `resolve::BodyBinder::bind_call_expr`), and a `None` classification
/// here doesn't guarantee the reference isn't *also* a still-unidentified
/// binder gap. That's an accepted tradeoff, not an oversight: the point
/// of `unresolved_reference_diagnostics` is full visibility into every
/// `Unresolved` reference, with severity as a confidence signal rather
/// than a filter -- see that function's own doc comment.
fn classify_unresolved(program: &BoundProgram, ptr: SyntaxPtr, name: &str) -> Option<&'static str> {
    match ptr.kind() {
        SyntaxKind::QualifiedName => Some(
            "catch-clause/switch-value/upsert-field type lookups resolve only against \
             project-local types, with no standard-library or schema fallback",
        ),
        // A built-in Apex exception *subtype* (`DmlException`,
        // `QueryException`, `NullPointerException`, ...) used as a type
        // reference (`System.DmlException caughtEx;`) -- Salesforce's own
        // docs only cover these in prose alongside `Exception` itself,
        // never as their own scraped class/method reference page (same
        // gap `Exception` itself had before this crate's own bundled
        // entry was hand-corrected -- see `apex_stdlib::standard_classes`'s
        // doc comment), so this crate has no way to confirm one by name
        // and never will without scraper changes. Every real Apex
        // exception class name ends in literally `Exception`, not just
        // convention -- a hard compiler rule (confirmed against a real
        // org: "Classes extending Exception must have a name ending in
        // Exception"), so this is a safe, general signal to classify by,
        // not a guess. Scoped to `Type` specifically (not `NameExpr`/
        // `FieldExpr`/...): an unrelated *variable* merely named
        // `somethingException` is not this shape at all.
        SyntaxKind::Type if name.ends_with("Exception") => Some(
            "the referenced name looks like a built-in Apex exception subtype (ends in \
             \"Exception\"), which this binder can never individually confirm -- Salesforce's \
             own docs cover these only in prose, never as their own class reference page",
        ),
        SyntaxKind::SuperExpr => Some(
            "this class's own supertype couldn't be resolved (often a standard exception \
             type or another unmodeled standard-library base)",
        ),
        SyntaxKind::SoqlFieldName => Some(
            "a SOQL field reference with no single object to resolve against (most often \
             a TYPEOF ... ELSE field, which by design applies across every non-matched type)",
        ),
        SyntaxKind::CallExpr => {
            let root = program.syntax(ptr.file());
            let is_this_or_super = ptr
                .to_node(&root)
                .and_then(CallExpr::cast)
                .and_then(|c| c.callee_token())
                .is_some_and(|t| matches!(t.kind(), SyntaxKind::This | SyntaxKind::Super));
            is_this_or_super.then_some(
                "an unqualified this(...)/super(...) constructor call on a type whose own \
                 inheritance couldn't be resolved",
            )
        }
        // Every node kind Pass 2 ever registers a `Resolution` under is
        // one of the ten matched here or listed explicitly below --
        // `BoundProgram::resolution_at`'s own doc comment confirms this
        // exhaustively by reading every `refs.set(...)` call site in
        // `resolve.rs`/`soql.rs`. Anything landing in this arm is
        // therefore never a node at all: it's a bare *token*-kind pointer
        // (`SyntaxPtr::for_token`), which -- per that constructor's own
        // doc comment -- only ever arises from `resolve::record_qualified_segments`,
        // one segment of a namespace-qualified type reference (e.g.
        // `System.Foo`). Deliberately matched by exclusion rather than by
        // a specific token kind (`SyntaxKind::Identifier`, say): a
        // namespace segment is very often a recognized keyword-shaped
        // token in its own right (`System`, `Schema`, `Database`, ...),
        // not a plain identifier -- matching only `Identifier` silently
        // missed exactly the most common real case (confirmed against a
        // real `System.String` reference, where `"System"`'s own token
        // kind is `SyntaxKind::System`, not `Identifier`).
        // The bare `Page` identifier in `Page.<name>` (a Visualforce page
        // reference) -- unlike `Label`/`Schema`, there is no real "Page"
        // class anywhere in Salesforce's own docs for this to resolve
        // against (confirmed: no such `apex_reference.json` entry), so it
        // stays `Unresolved` by design even when the *whole* `Page.<name>`
        // reference resolves correctly one level up
        // (`resolve::bind_field_expr`'s own doc comment on this exact
        // shape). Scoped specifically to a `NameExpr` whose own parent is
        // a `FieldExpr` (the receiver position) so an unrelated variable
        // that happens to be named `page` doesn't get misclassified.
        SyntaxKind::NameExpr => {
            let root = program.syntax(ptr.file());
            let is_page_namespace_prefix = ptr.to_node(&root).and_then(NameExpr::cast).is_some_and(|ne| {
                ne.name_token().is_some_and(|t| t.text().eq_ignore_ascii_case("Page"))
                    && ne.syntax().parent().is_some_and(|p| p.kind() == SyntaxKind::FieldExpr)
            });
            is_page_namespace_prefix.then_some(
                "the bare `Page` identifier is pure compiler-magic Visualforce-page-reference \
                 syntax with no real declaration of its own -- only the page name after the dot \
                 needs to exist",
            )
        }
        SyntaxKind::FieldExpr
        | SyntaxKind::Type
        | SyntaxKind::MethodCallExpr
        | SyntaxKind::NewExpr
        | SyntaxKind::ThisExpr => None,
        _ => Some(
            "one segment of a namespace-qualified type reference (e.g. `System.Foo`) -- \
             the whole reference may still resolve correctly",
        ),
    }
}

/// `textDocument/publishDiagnostics`: one diagnostic per reference the
/// binder recorded as `Resolution::Unresolved` (`resolutions_in_file`) --
/// deliberately *every* one, not a subset picked for precision. An
/// earlier design only surfaced the highest-confidence shapes (a bare
/// name) and quietly dropped the rest; that hid real information the
/// project itself needs, since every one of `resolution_regression_baseline.rs`'s
/// `28,172`-and-counting `Unresolved` references on the real NPSP corpus
/// is either a genuine bug in someone's Apex or a gap in this binder's own
/// modeling -- and the only way to keep closing those gaps
/// (`examples/unresolved_clusters.rs`'s whole purpose, run offline today)
/// is to keep seeing where they are. So instead of filtering, this grades
/// confidence via severity: `classify_unresolved` recognizes the specific
/// shapes already known to be a binder limitation rather than a code
/// defect and reports those as `WARNING` with a message explaining why;
/// everything else -- the more likely-a-real-bug default, though not a
/// guarantee, see `classify_unresolved`'s own doc comment -- is `ERROR`.
/// Range and message text both come from `BoundProgram::highlight_range`,
/// which already narrows every reference kind here to its tight
/// identifier span (its own doc comment covers all of them); slicing the
/// file's raw text at that same range is simpler and more uniform than a
/// separate per-`SyntaxKind` AST accessor for "the name text."
pub(crate) fn unresolved_reference_diagnostics(
    program: &BoundProgram,
    file: FileId,
    encoding: PositionEncoding,
) -> Vec<Diagnostic> {
    let text = program.syntax(file).text().to_string();
    let index = LineIndex::new(&text);
    program
        .resolutions_in_file(file)
        .filter(|(_, res)| matches!(res, Resolution::Unresolved))
        .map(|(ptr, _)| {
            let range = program.highlight_range(*ptr);
            let name = &text[usize::from(range.start())..usize::from(range.end())];
            let lsp_range = Range {
                start: index.to_position(&text, range.start().into(), encoding),
                end: index.to_position(&text, range.end().into(), encoding),
            };
            let (severity, message) = match classify_unresolved(program, *ptr, name) {
                Some(reason) => (
                    DiagnosticSeverity::WARNING,
                    format!(
                        "unresolved reference to '{name}' -- likely an apexls limitation \
                         ({reason}), not necessarily invalid Apex"
                    ),
                ),
                None => (
                    DiagnosticSeverity::ERROR,
                    format!("cannot resolve reference to '{name}'"),
                ),
            };
            Diagnostic {
                range: lsp_range,
                severity: Some(severity),
                source: Some("apexls".to_string()),
                message,
                ..Default::default()
            }
        })
        .collect()
}

/// `textDocument/publishDiagnostics`: one `ERROR`-severity diagnostic per
/// `Resolution::UnknownSchema` recorded for `file` -- already computed
/// during Pass 2 (`crate::soql`/`schema_index::resolve_object`) whenever a
/// SOQL/SOSL query names an object or field with no matching local or
/// standard schema entry (`FROM Unknown_Object__c`, a bad
/// `WHERE`/`SELECT`/`ORDER BY` field, an unresolvable `TYPEOF ... WHEN`
/// type). Same story as `unresolved_reference_diagnostics`: purely
/// surfacing data the binder already computes, no new analysis.
///
/// Scoped specifically to `ptr.kind() == SyntaxKind::SoqlFieldName` --
/// every genuine SOQL/SOSL object-or-field reference (`SoqlFromList`'s
/// entries, a select/where/order-by/group-by field name, a
/// `SoslFieldSpec`'s object, a `TYPEOF ... WHEN`'s type name) is, per
/// `apex-syntax`'s own AST (`SoqlFromList::entries`/`SoslFieldSpec::object`
/// both literally return `SoqlFieldName`), that one node kind --
/// deliberately excluding `Resolution::UnknownSchema`'s two *other*
/// producers in `crate::resolve`, neither of which is SOQL or an error:
/// `bind_field_expr`'s `<Object>.fields`/`<Object>.fields.<Field>`
/// describe-token hop (`fields` is real, compiler-magic Apex syntax, not a
/// genuine field -- surfacing it here would misfire on every legitimate
/// `Schema.SObjectField f = Account.fields.Name;`-shaped expression, a
/// guaranteed false positive) and `bind_sobject_field_init`'s SObject-
/// constructor field-init check (a real, separate gap, out of this
/// diagnostic's scope -- see the Wayfinder map's fog for the follow-on).
pub(crate) fn unknown_schema_diagnostics(
    program: &BoundProgram,
    file: FileId,
    encoding: PositionEncoding,
) -> Vec<Diagnostic> {
    let text = program.syntax(file).text().to_string();
    let index = LineIndex::new(&text);
    program
        .resolutions_in_file(file)
        .filter(|(ptr, res)| {
            ptr.kind() == SyntaxKind::SoqlFieldName && matches!(res, Resolution::UnknownSchema(_))
        })
        .map(|(ptr, res)| {
            let Resolution::UnknownSchema(schema_ref) = res else {
                unreachable!("filtered to Resolution::UnknownSchema above")
            };
            let range = program.highlight_range(*ptr);
            let lsp_range = Range {
                start: index.to_position(&text, range.start().into(), encoding),
                end: index.to_position(&text, range.end().into(), encoding),
            };
            let message = match (&schema_ref.object, &schema_ref.field) {
                (Some(object), Some(field)) => {
                    format!("'{field}' is not a valid field on object '{object}'")
                }
                (Some(object), None) => format!("'{object}' is not a valid object"),
                (None, Some(field)) => format!("'{field}' is not a valid field"),
                (None, None) => "unresolvable schema reference".to_string(),
            };
            Diagnostic {
                range: lsp_range,
                severity: Some(DiagnosticSeverity::ERROR),
                source: Some("apexls".to_string()),
                message,
                ..Default::default()
            }
        })
        .collect()
}

/// `SyntaxKind`s `apex_syntax::ast::decl::HasModifiers` is actually
/// implemented for -- every declaration shape that can carry `modifier*`.
const MODIFIER_BEARING_KINDS: &[SyntaxKind] = &[
    SyntaxKind::ClassDecl,
    SyntaxKind::InterfaceDecl,
    SyntaxKind::EnumDecl,
    SyntaxKind::MethodDecl,
    SyntaxKind::ConstructorDecl,
    SyntaxKind::FieldDecl,
    SyntaxKind::PropertyDecl,
    SyntaxKind::FormalParam,
];

fn is_visibility_modifier_kind(kind: SyntaxKind) -> bool {
    matches!(
        kind,
        SyntaxKind::Public | SyntaxKind::Private | SyntaxKind::Protected | SyntaxKind::Global
    )
}

/// `textDocument/publishDiagnostics`: one `ERROR`-severity diagnostic per
/// duplicate or conflicting modifier on a single declaration -- confirmed
/// a real, *semantic* (deploy-time) compiler error against a real
/// Salesforce org, not a syntax-level one (Wayfinder `apex-diagnostics`
/// map, ticket 02's research): `apex-parser`'s grammar deliberately keeps
/// parsing `modifier*` as an unconstrained repetition, matching the real
/// org's own grammar (which accepts the same repetition syntactically and
/// only rejects it in a later compiler phase), so this stays a semantic
/// check here rather than a parser-level restriction -- changing the
/// grammar would diverge from real Apex and would also change error
/// recovery/AST shape for in-progress, syntactically-tolerant text an LSP
/// needs to keep serving other features against.
///
/// Walks every `MODIFIER_BEARING_KINDS` node directly off the raw syntax
/// tree via `apex_syntax::ast::support::children::<Modifier>`, which
/// works on any node regardless of its specific typed wrapper -- so this
/// doesn't need `HasModifiers`'s per-type trait dispatch, nor the
/// binder's already-collapsed `apex_binder::symbol::ModifierSet` (which
/// idempotently discards duplicates, `crates/apex-binder/src/symbol.rs:123-148`
/// -- exactly the information this diagnostic needs to still see). Two
/// rules, matching the two shapes generalized from what was actually
/// confirmed against a real org:
/// - Any modifier keyword repeated on the same declaration -> `Duplicate
///   modifier: <keyword>` on each repeat past the first (real message,
///   confirmed for `private`/`static`: `"Duplicate modifier: private"`;
///   generalized here to any keyword, since the underlying rule -- a real
///   compiler rejecting a repeated modifier -- isn't specific to which
///   one it is).
/// - More than one distinct visibility keyword (`public`/`private`/
///   `protected`/`global`) on the same declaration -> `Declarations can
///   only have one scope` on each one past the first-seen (real message,
///   verbatim).
/// A third confirmed shape (`static`+`abstract` -> `"static methods
/// cannot be abstract"`) is scoped to `MethodDecl` only, deliberately not
/// generalized: that's the only shape actually verified against a real
/// org, and `abstract` isn't otherwise meaningful on a field/property/
/// parameter, so there's no real case elsewhere for it to fire on anyway.
pub(crate) fn modifier_diagnostics(
    program: &BoundProgram,
    file: FileId,
    encoding: PositionEncoding,
) -> Vec<Diagnostic> {
    let root = program.syntax(file);
    let text = root.text().to_string();
    let index = LineIndex::new(&text);

    let diagnostic = |range: TextRange, message: String| Diagnostic {
        range: Range {
            start: index.to_position(&text, range.start().into(), encoding),
            end: index.to_position(&text, range.end().into(), encoding),
        },
        severity: Some(DiagnosticSeverity::ERROR),
        source: Some("apexls".to_string()),
        message,
        ..Default::default()
    };

    let mut diagnostics = Vec::new();
    for node in root.descendants() {
        if !MODIFIER_BEARING_KINDS.contains(&node.kind()) {
            continue;
        }
        let modifiers: Vec<_> = support::children::<Modifier>(&node)
            .filter_map(|m| m.keyword())
            .collect();

        let mut seen_counts: HashMap<SyntaxKind, u32> = HashMap::new();
        let mut first_visibility: Option<SyntaxKind> = None;
        for tok in &modifiers {
            let count = seen_counts.entry(tok.kind()).or_insert(0);
            *count += 1;
            if *count > 1 {
                diagnostics.push(diagnostic(tok.text_range(), format!("Duplicate modifier: {}", tok.text())));
                continue;
            }
            if is_visibility_modifier_kind(tok.kind()) {
                match first_visibility {
                    None => first_visibility = Some(tok.kind()),
                    Some(first) if first != tok.kind() => {
                        diagnostics.push(diagnostic(tok.text_range(), "Declarations can only have one scope".to_string()));
                    }
                    _ => {}
                }
            }
        }

        if node.kind() == SyntaxKind::MethodDecl && modifiers.iter().any(|t| t.kind() == SyntaxKind::Static) {
            if let Some(abstract_tok) = modifiers.iter().find(|t| t.kind() == SyntaxKind::Abstract) {
                diagnostics.push(diagnostic(abstract_tok.text_range(), "static methods cannot be abstract".to_string()));
            }
        }
    }
    diagnostics
}

/// Names, case-insensitively, every `Database.<method>` call this
/// diagnostic treats the same as its keyword-statement equivalent --
/// mirrors exactly the DML statement kinds/dynamic-SOQL entry points
/// `crate::resolve` (`crates/apex-binder/src/resolve.rs:3070`) already
/// recognizes by the identical textual-receiver-name pattern for dynamic-
/// SOQL bind resolution.
const DATABASE_BULK_METHOD_NAMES: &[&str] = &[
    "insert",
    "update",
    "delete",
    "upsert",
    "undelete",
    "merge",
    "query",
    "countquery",
    "getquerylocator",
];

/// Whether `node`'s nearest enclosing loop, if any, wraps it through that
/// loop's own *body* -- not its condition/init/update/iterable, all of
/// which run zero or one time per loop execution rather than once per
/// iteration. This distinction is load-bearing, not pedantic: Apex's
/// canonical bulkified idiom, the SOQL-for-loop (`for (Account a :
/// [SELECT ... FROM Account]) { ... }`), embeds its query as the
/// `ForEachStmt`'s own `iterable()` -- evaluated exactly *once*, before
/// the loop starts, never per iteration. Treating that position the same
/// as the loop body would flag the single most common *correctly*
/// bulkified real-world pattern as if it were the anti-pattern it's
/// specifically written to avoid.
///
/// Keeps climbing past a loop whose non-body position `node` came through
/// (rather than stopping there) so a query embedded in one loop's
/// iterable/condition that itself sits inside an *outer* loop's body
/// still correctly counts -- e.g. `for (a : accounts) { for (c : [SELECT
/// ... WHERE AccountId = :a.Id]) { ... } }` really is the classic N+1
/// anti-pattern (the inner query re-runs once per outer iteration), even
/// though the inner query's own immediate loop only evaluates it once.
fn is_inside_loop_body(node: &SyntaxNode) -> bool {
    let mut current = node.clone();
    while let Some(parent) = current.parent() {
        let body = match parent.kind() {
            SyntaxKind::ForStmt => ForStmt::cast(parent.clone()).and_then(|s| s.body()).map(|b| b.syntax().clone()),
            SyntaxKind::ForEachStmt => ForEachStmt::cast(parent.clone()).and_then(|s| s.body()).map(|b| b.syntax().clone()),
            SyntaxKind::WhileStmt => WhileStmt::cast(parent.clone()).and_then(|s| s.body()).map(|b| b.syntax().clone()),
            SyntaxKind::DoWhileStmt => DoWhileStmt::cast(parent.clone()).and_then(|s| s.body()).map(|b| b.syntax().clone()),
            _ => None,
        };
        if body.is_some_and(|b| b == current) {
            return true;
        }
        current = parent;
    }
    false
}

/// `textDocument/publishDiagnostics`: one `WARNING`-severity diagnostic
/// per DML statement or SOQL/SOSL query found inside a loop body -- the
/// classic Salesforce governor-limit "bulkification" anti-pattern
/// (Wayfinder `apex-diagnostics` map, ticket 04's design). `WARNING`, not
/// `ERROR`: unlike every other diagnostic this server ships, this is a
/// *runtime* risk (`System.LimitException` only past a real data-volume
/// threshold), not a certain compile-time defect -- the flagged code
/// compiles and often runs fine.
///
/// Deliberately unconditional once a candidate is confirmed inside a
/// loop's own body (see [`is_inside_loop_body`]): no exemption for a loop
/// "provably" single-iteration, and none for the DML/query's own operand
/// shape -- there's no legitimate already-bulk escape hatch, since the
/// anti-pattern is the *statement* executing once per iteration
/// regardless of any one call's row count. Purely syntactic, needing no
/// new binder capability (matching this whole diagnostic's own design
/// decision not to build interprocedural/data-flow analysis for it):
/// three candidate shapes, found directly off the raw syntax tree --
/// - A keyword-form DML statement (`InsertStmt`/`UpdateStmt`/`DeleteStmt`/
///   `UndeleteStmt`/`UpsertStmt`/`MergeStmt`).
/// - A SOQL/SOSL query expression (`SoqlExpr`/`SoslExpr`) -- wherever it
///   appears as an expression, e.g. a `[SELECT ...]` bracket literal.
///   Excludes a `ForEachStmt`'s own `iterable()` position via
///   `is_inside_loop_body`'s own body-vs-non-body distinction, so the
///   canonical SOQL-for-loop idiom itself is never flagged.
/// - A programmatic `Database.<method>` call (`DATABASE_BULK_METHOD_NAMES`)
///   -- recognized the same way `crate::resolve`'s dynamic-SOQL bind
///   resolution already recognizes `Database.query`/`countQuery`/
///   `getQueryLocator`, a plain textual check that the call's receiver is
///   a bare `Database` name (case-insensitive) -- deliberately not routed
///   through real type resolution (`Ty`'s per-expression chaining is
///   walker-internal only, never stored on `BoundProgram`, see
///   `crate::ty`'s own doc comment), an accepted, negligible-risk
///   simplification: no real Apex project has its own class literally
///   named `Database` to shadow the stdlib one.
///
/// Cross-method (interprocedural) bulkification -- a loop calling a
/// helper method that itself issues DML/SOQL -- is a real, separate
/// question the design ticket explicitly split out (needs new call-graph
/// machinery this binder has nowhere today); not attempted here.
pub(crate) fn bulkification_diagnostics(
    program: &BoundProgram,
    file: FileId,
    encoding: PositionEncoding,
) -> Vec<Diagnostic> {
    let root = program.syntax(file);
    let text = root.text().to_string();
    let index = LineIndex::new(&text);

    let diagnostic = |range: TextRange, message: String| Diagnostic {
        range: Range {
            start: index.to_position(&text, range.start().into(), encoding),
            end: index.to_position(&text, range.end().into(), encoding),
        },
        severity: Some(DiagnosticSeverity::WARNING),
        source: Some("apexls".to_string()),
        message,
        ..Default::default()
    };

    let mut diagnostics = Vec::new();
    for node in root.descendants() {
        let (range, keyword) = match node.kind() {
            SyntaxKind::InsertStmt
            | SyntaxKind::UpdateStmt
            | SyntaxKind::DeleteStmt
            | SyntaxKind::UndeleteStmt
            | SyntaxKind::UpsertStmt
            | SyntaxKind::MergeStmt => {
                let keyword = node.first_token().map(|t| t.text().to_string()).unwrap_or_default();
                (node.text_range(), format!("'{keyword}' statement"))
            }
            SyntaxKind::SoqlExpr | SyntaxKind::SoslExpr => (node.text_range(), "SOQL/SOSL query".to_string()),
            SyntaxKind::MethodCallExpr => {
                let Some(mc) = MethodCallExpr::cast(node.clone()) else {
                    continue;
                };
                let is_database_call = matches!(mc.target(), Some(Expr::Name(n)) if n
                    .name_token()
                    .is_some_and(|t| t.text().eq_ignore_ascii_case("Database")));
                let Some(method_tok) = mc.method_name_token() else {
                    continue;
                };
                if !is_database_call
                    || !DATABASE_BULK_METHOD_NAMES.contains(&method_tok.text().to_ascii_lowercase().as_str())
                {
                    continue;
                }
                (method_tok.text_range(), format!("'Database.{}' call", method_tok.text()))
            }
            _ => continue,
        };
        if is_inside_loop_body(&node) {
            diagnostics.push(diagnostic(
                range,
                format!("{keyword} inside a loop may exceed governor limits -- move it outside the loop and operate on a bulk collection instead"),
            ));
        }
    }
    diagnostics
}

/// Whether every statement in `block` runs, in order, at least once --
/// i.e. whether `block` itself definitely terminates (see
/// [`stmt_terminates`]'s own doc comment for what that means). Factored
/// out since three different callers all reduce to "does this `Option<Block>`
/// terminate": `Stmt::Block` itself, a `TryStmt`'s `finally_clause().body()`,
/// a `SwitchStmt` arm's `WhenClause::body()`, and a `DoWhileStmt`'s
/// `body()` -- all four are `Block`, not `Stmt`, at the AST level.
fn block_terminates(block: &Block) -> bool {
    block.statements().any(|s| stmt_terminates(&s))
}

/// Whether `stmt` *definitely* transfers control away rather than
/// falling through to whatever follows it -- the recursive predicate
/// behind `unreachable_code_diagnostics` (Wayfinder `apex-diagnostics`
/// map, ticket 05's/ticket 14's design). `ForStmt`/`ForEachStmt`/
/// `WhileStmt` still fall to the conservative `_ => false` default
/// unconditionally -- their conditional entry (the body may run zero
/// times) makes treating them as terminators unsound regardless of what
/// their body does, and this binder has no value-range/data-flow
/// analysis to ever rule that out.
fn stmt_terminates(stmt: &Stmt) -> bool {
    match stmt {
        Stmt::Return(_) | Stmt::Throw(_) | Stmt::Break(_) | Stmt::Continue(_) => true,
        // A block terminates as soon as *any* statement in it does --
        // once reached, nothing after that statement (inside this same
        // block) ever executes, so it doesn't matter whether the
        // terminator is the block's last statement or an earlier one.
        Stmt::Block(block) => block_terminates(block),
        // Only an `if` with an `else` where *both* branches terminate is
        // itself a terminator -- no `else`, or a branch that can fall
        // through, means control can still reach past the whole `if`.
        Stmt::If(if_stmt) => {
            let Some(then_branch) = if_stmt.then_branch() else {
                return false;
            };
            let Some(else_branch) = if_stmt.else_branch() else {
                return false;
            };
            stmt_terminates(&then_branch) && stmt_terminates(&else_branch)
        }
        // `finally` always runs, regardless of how the `try` body or any
        // `catch` clause completes -- so a `finally` that itself
        // terminates makes the whole `try` terminate too, independent of
        // everything else in it. No `finally` at all (or one that
        // doesn't terminate) -> never a terminator: this deliberately
        // doesn't attempt full try/catch definite-completion analysis
        // (would the try body and every catch clause need to terminate?
        // too gnarly a real false-positive risk for the payoff, per
        // ticket 14's own decision).
        Stmt::Try(try_stmt) => try_stmt.finally_clause().and_then(|f| f.body()).is_some_and(|b| block_terminates(&b)),
        // Genuinely exhaustive (a `when else` arm is present -- Apex's
        // `switch on` doesn't fall through between arms, so exhaustiveness
        // is the only way every real path is covered) AND every arm's
        // own body terminates.
        Stmt::Switch(switch_stmt) => {
            let mut clauses = switch_stmt.when_clauses().peekable();
            if clauses.peek().is_none() {
                return false;
            }
            let has_else_arm = switch_stmt
                .when_clauses()
                .any(|w| w.value().is_some_and(|v| v.is_else()));
            has_else_arm
                && clauses.all(|w| w.body().is_some_and(|b| block_terminates(&b)))
        }
        // Sound specifically because a `do`-`while` body unconditionally
        // runs at least once, unlike `for`/`foreach`/`while`'s
        // conditional entry -- see this function's own doc comment for
        // why those three stay `false` unconditionally.
        Stmt::DoWhile(do_while) => do_while.body().is_some_and(|b| block_terminates(&b)),
        _ => false,
    }
}

/// `textDocument/publishDiagnostics`: one `ERROR`-severity diagnostic per
/// statement that's unreachable because an earlier statement in the same
/// block definitely terminates control flow first (Wayfinder
/// `apex-diagnostics` map, ticket 05's design/ticket 13's
/// implementation). `ERROR`, not `WARNING`: unlike bulkification, this is
/// a certain, provable defect once flagged -- the statement genuinely
/// cannot execute, full stop, matching `unknown_schema_diagnostics`/
/// `modifier_diagnostics`'s confidence level.
///
/// Walks every `Block` node in the file independently (so a block nested
/// inside a `TryStmt`/`SwitchStmt`/loop/`IfStmt` branch that itself isn't
/// treated as a terminator still gets its *own* unreachable code found --
/// see [`stmt_terminates`]'s own doc comment for why those container
/// kinds are conservatively never terminators themselves), scanning its
/// direct statement children in order: the first one [`stmt_terminates`]
/// confirms terminates flips every statement after it, in that same
/// block, into "unreachable."
pub(crate) fn unreachable_code_diagnostics(
    program: &BoundProgram,
    file: FileId,
    encoding: PositionEncoding,
) -> Vec<Diagnostic> {
    let root = program.syntax(file);
    let text = root.text().to_string();
    let index = LineIndex::new(&text);

    let diagnostic = |range: TextRange| Diagnostic {
        range: Range {
            start: index.to_position(&text, range.start().into(), encoding),
            end: index.to_position(&text, range.end().into(), encoding),
        },
        severity: Some(DiagnosticSeverity::ERROR),
        source: Some("apexls".to_string()),
        message: "unreachable statement".to_string(),
        ..Default::default()
    };

    let mut diagnostics = Vec::new();
    for node in root.descendants() {
        if node.kind() != SyntaxKind::Block {
            continue;
        }
        let Some(block) = Block::cast(node) else {
            continue;
        };
        let mut past_terminator = false;
        for stmt in block.statements() {
            if past_terminator {
                diagnostics.push(diagnostic(stmt.syntax().text_range()));
            } else if stmt_terminates(&stmt) {
                past_terminator = true;
            }
        }
    }
    diagnostics
}

/// Whether some method reachable from `class_id` (the class itself, or
/// any ancestor in its `inherited_chain`) already provides the required
/// `(name_lower, arity)` signature -- the satisfaction half of
/// `missing_implementation_diagnostics` (Wayfinder `apex-diagnostics`
/// map, ticket 07's design/ticket 17's implementation). Two different
/// rules collapse into one scan, gated by `needs_override` (per-key, set
/// by whichever ancestor(s) require it -- see that function's own
/// `required` map construction): an interface-required method just needs
/// *any* real (non-abstract) same-name/same-arity method anywhere in the
/// chain; an abstract-superclass-required one needs `modifiers.is_override`
/// specifically, matching Apex's own real asymmetry (confirmed against
/// this project's existing `dynamic_dispatch_resolution.rs` fixtures: no
/// real Apex requires `override` to satisfy an `implements`, only an
/// `extends`). An interface's own member is never itself a satisfier
/// (it's a requirement, not a provision) -- excluded by skipping any
/// container whose own `kind` is `Interface` before scanning its members.
fn has_required_override(
    program: &BoundProgram,
    class_id: SymbolId,
    name_lower: &str,
    arity: usize,
    needs_override: bool,
) -> bool {
    let mut containers = vec![class_id];
    containers.extend(program.symbols.inherited_chain(class_id).iter().copied());
    containers.into_iter().any(|container_id| {
        if program.symbols.get(container_id).kind == SymbolKind::Interface {
            return false;
        }
        program.symbols.members_of(container_id).iter().any(|&member_id| {
            let member = program.symbols.get(member_id);
            member.kind == SymbolKind::Method
                && !member.modifiers.is_abstract
                && member.name.eq_ignore_ascii_case(name_lower)
                && program.symbols.params(member_id).len() == arity
                && (!needs_override || member.modifiers.is_override)
        })
    })
}

/// `textDocument/publishDiagnostics`: one `ERROR`-severity diagnostic per
/// abstract/interface method a concrete project-local class fails to
/// implement (Wayfinder `apex-diagnostics` map, ticket 07's design/
/// ticket 17's implementation). Scoped to **project-local** interfaces/
/// abstract classes only -- `SymbolTable` never resolves a standard-
/// library `implements` target (`Comparable`, `Database.Batchable`, ...)
/// into `inherited_chain` at all (confirmed in ticket 06's own research),
/// so this check simply never sees those requirements; a class correctly
/// implementing (or botching) a stdlib interface is silently outside its
/// scope either way, never a false positive (see
/// `crates/apex-diagnostics/issues/18-...` for the follow-on that would
/// extend coverage there, which needs real `apex-binder`-core changes
/// this ticket deliberately doesn't attempt).
///
/// Bridges from the syntax tree to the symbol table via
/// `BoundProgram::symbol_at` (a `ClassDecl`'s own name-token offset ->
/// its `SymbolId`) rather than any `pub(crate)`-only per-file symbol
/// listing, since this diagnostic -- unlike every other one in this
/// file -- needs real symbol-table data (`inherited_chain`/`members_of`/
/// `params`), not just the raw syntax tree.
///
/// For each concrete (non-abstract) class, walks `inherited_chain` once
/// to build a `(name, arity) -> requirement` map (a method is "required"
/// when `is_abstract || its container is an Interface`, since interface
/// methods are implicitly abstract without the modifier bit ever being
/// set -- ticket 06's own finding), tightening `needs_override` to `true`
/// only for an abstract-class-declared requirement whose own visibility
/// is explicit (`Public`/`Protected`/`Global`) -- confirmed against a
/// real org, not assumed: `abstract String run();` with **no** visibility
/// keyword (a real, common shape -- `fflib_SObjectSelector.cls`'s own
/// `getSObjectType`/`getSObjectFieldList`, both real NPSP code) can be
/// overridden with no `override` keyword and deploys cleanly, while the
/// identical shape with an explicit `public abstract` modifier is
/// rejected with `"Method must use the override keyword"` if the
/// override omits it. `ModifierSet`'s own `Visibility::default()` is
/// `Private` (`crates/apex-binder/src/symbol.rs:73-79`), which is
/// exactly the value an unmodified declaration already collapses to, so
/// this reuses that existing signal rather than needing a new one.
/// Interface-declared requirements never need `override` regardless
/// (unaffected by this). When a same-signature requirement comes from
/// *both* an interface and an abstract class (a rare collision), the
/// stricter (override-requiring) rule wins, the conservative choice.
/// Method-level dedup across multiple interfaces requiring the same
/// name+arity falls out for free from keying by `(name, arity)` rather
/// than per-ancestor.
pub(crate) fn missing_implementation_diagnostics(
    program: &BoundProgram,
    file: FileId,
    encoding: PositionEncoding,
) -> Vec<Diagnostic> {
    let root = program.syntax(file);
    let text = root.text().to_string();
    let index = LineIndex::new(&text);

    let diagnostic = |range: TextRange, message: String| Diagnostic {
        range: Range {
            start: index.to_position(&text, range.start().into(), encoding),
            end: index.to_position(&text, range.end().into(), encoding),
        },
        severity: Some(DiagnosticSeverity::ERROR),
        source: Some("apexls".to_string()),
        message,
        ..Default::default()
    };

    let mut diagnostics = Vec::new();
    for node in root.descendants() {
        if node.kind() != SyntaxKind::ClassDecl {
            continue;
        }
        let Some(class_decl) = ClassDecl::cast(node) else {
            continue;
        };
        let Some(name_tok) = class_decl.name().and_then(|n| n.token()) else {
            continue;
        };
        let Some(class_id) = program.symbol_at(file, name_tok.text_range().start()) else {
            continue;
        };
        let symbol = program.symbols.get(class_id);
        if symbol.kind != SymbolKind::Class || symbol.modifiers.is_abstract {
            continue;
        }

        // (name_lower, arity) -> (needs_override, ancestor's own name, method's own declared-case name)
        let mut required: HashMap<(String, usize), (bool, String, String)> = HashMap::new();
        for &ancestor_id in program.symbols.inherited_chain(class_id) {
            let ancestor = program.symbols.get(ancestor_id);
            let from_interface = ancestor.kind == SymbolKind::Interface;
            for &member_id in program.symbols.members_of(ancestor_id) {
                let member = program.symbols.get(member_id);
                if member.kind != SymbolKind::Method || !(member.modifiers.is_abstract || from_interface) {
                    continue;
                }
                let arity = program.symbols.params(member_id).len();
                let key = (member.name.to_ascii_lowercase(), arity);
                let needs_override = !from_interface && member.modifiers.visibility != Visibility::Private;
                required
                    .entry(key)
                    .and_modify(|(existing, _, _)| *existing |= needs_override)
                    .or_insert((needs_override, ancestor.name.to_string(), member.name.to_string()));
            }
        }

        for ((name_lower, arity), (needs_override, ancestor_name, method_name)) in &required {
            if has_required_override(program, class_id, name_lower, *arity, *needs_override) {
                continue;
            }
            diagnostics.push(diagnostic(
                name_tok.text_range(),
                format!("'{}' does not implement '{method_name}' required by '{ancestor_name}'", symbol.name),
            ));
        }
    }
    diagnostics
}

/// `textDocument/publishDiagnostics`: one `ERROR`-severity diagnostic per
/// `apex_binder::TypeMismatch` recorded for `file` -- already computed
/// inline during Pass 2 (Wayfinder `apex-diagnostics` map, ticket 09's
/// Option B design/ticket 23's implementation: three checkpoints --
/// a local variable's declared type vs. its initializer, a `return`'s
/// declared method return type vs. the returned expression, and a
/// resolved call/`new` expression's declared parameter types vs. its
/// arguments -- routed through the same, already org-verified
/// `conversions::type_compatible` every existing overload-resolution/
/// dead-code check already relies on). Same story as every other
/// already-computed-elsewhere diagnostic in this file: purely surfacing
/// data the binder already produced, no new analysis here.
fn visibility_keyword(visibility: Visibility) -> &'static str {
    match visibility {
        Visibility::Private => "private",
        Visibility::Protected => "protected",
        Visibility::Public => "public",
        Visibility::Global => "global",
    }
}

fn narrowing_kind_label(kind: SymbolKind) -> &'static str {
    match kind {
        SymbolKind::Method => "Method",
        SymbolKind::Field => "Field",
        SymbolKind::Property => "Property",
        SymbolKind::Constructor => "Constructor",
        _ => "Declaration",
    }
}

/// `textDocument/publishDiagnostics`: one `WARNING`-severity diagnostic
/// per member `apex_binder::narrowing_candidates_in_file` proves is
/// declared more broadly than its real usage needs -- the mirror image of
/// `dead_code_diagnostics` (used-nowhere): this is "used, but too
/// broadly." No `DiagnosticTag`: unlike a dead symbol, nothing here is
/// safe to delete, so `UNNECESSARY` (rust-analyzer's own "safe to remove"
/// tag) would be the wrong signal.
pub(crate) fn visibility_narrowing_diagnostics(
    program: &BoundProgram,
    file: FileId,
    encoding: PositionEncoding,
) -> Vec<Diagnostic> {
    let text = program.syntax(file).text().to_string();
    let index = LineIndex::new(&text);
    apex_binder::narrowing_candidates_in_file(program, file)
        .into_iter()
        .map(|candidate| {
            let range = Range {
                start: index.to_position(&text, candidate.name_range.start().into(), encoding),
                end: index.to_position(&text, candidate.name_range.end().into(), encoding),
            };
            Diagnostic {
                range,
                severity: Some(DiagnosticSeverity::WARNING),
                source: Some("apexls".to_string()),
                message: format!(
                    "{} '{}' is declared '{}' but could be '{}'",
                    narrowing_kind_label(candidate.kind),
                    candidate.name,
                    visibility_keyword(candidate.current),
                    visibility_keyword(candidate.required),
                ),
                ..Default::default()
            }
        })
        .collect()
}

pub(crate) fn type_mismatch_diagnostics(
    program: &BoundProgram,
    file: FileId,
    encoding: PositionEncoding,
) -> Vec<Diagnostic> {
    let text = program.syntax(file).text().to_string();
    let index = LineIndex::new(&text);
    program
        .type_mismatches(file)
        .iter()
        .map(|tm| {
            let range = program.highlight_range(tm.ptr);
            Diagnostic {
                range: Range {
                    start: index.to_position(&text, range.start().into(), encoding),
                    end: index.to_position(&text, range.end().into(), encoding),
                },
                severity: Some(DiagnosticSeverity::ERROR),
                source: Some("apexls".to_string()),
                message: tm.message.clone(),
                ..Default::default()
            }
        })
        .collect()
}

/// `textDocument/publishDiagnostics`: one `WARNING`-severity diagnostic
/// per symbol `apex_binder::dead_symbols_in_file` proves is dead, tagged
/// `DiagnosticTag::UNNECESSARY` -- the standard LSP tag for "safe to
/// remove," which clients render faded/strikethrough independent of the
/// warning squiggle (rust-analyzer's own `dead_code` diagnostics use the
/// same tag).
pub(crate) fn dead_code_diagnostics(
    program: &BoundProgram,
    file: FileId,
    encoding: PositionEncoding,
) -> Vec<Diagnostic> {
    let text = program.syntax(file).text().to_string();
    let index = LineIndex::new(&text);
    apex_binder::dead_symbols_in_file(program, file)
        .into_iter()
        .map(|dead| {
            let range = Range {
                start: index.to_position(&text, dead.name_range.start().into(), encoding),
                end: index.to_position(&text, dead.name_range.end().into(), encoding),
            };
            Diagnostic {
                range,
                severity: Some(DiagnosticSeverity::WARNING),
                source: Some("apexls".to_string()),
                message: format!(
                    "{} '{}' is never used",
                    apex_binder::kind_label(dead.kind, dead.visibility),
                    dead.name
                ),
                tags: Some(vec![DiagnosticTag::UNNECESSARY]),
                ..Default::default()
            }
        })
        .collect()
}

fn ranges_overlap(a: Range, b: Range) -> bool {
    fn le(x: Position, y: Position) -> bool {
        (x.line, x.character) <= (y.line, y.character)
    }
    le(a.start, b.end) && le(b.start, a.end)
}

/// `textDocument/codeAction`: a "Remove unused ..." quick-fix for every
/// dead symbol (`dead_symbols_in_file`) whose own name overlaps the
/// requested `range`. Deliberately re-derives dead symbols from
/// `program` rather than trusting `params.context.diagnostics` echoed
/// back by the client -- self-contained, and works even if the client
/// never displayed/requested `dead_code_diagnostics` first. Computes the
/// `WorkspaceEdit` eagerly rather than deferring to `codeAction/resolve`
/// (unimplemented): cheap, one symbol, `program` already loaded in
/// memory -- the same eager-computation choice `rename_edits` already
/// makes.
pub(crate) fn dead_code_actions(
    program: &BoundProgram,
    file: FileId,
    range: Range,
    encoding: PositionEncoding,
) -> Vec<CodeActionOrCommand> {
    let Ok(uri) = Url::from_file_path(program.file_path(file)) else {
        return Vec::new();
    };
    let text = program.syntax(file).text().to_string();
    let index = LineIndex::new(&text);
    apex_binder::dead_symbols_in_file(program, file)
        .into_iter()
        .filter(|dead| {
            let name_range = Range {
                start: index.to_position(&text, dead.name_range.start().into(), encoding),
                end: index.to_position(&text, dead.name_range.end().into(), encoding),
            };
            ranges_overlap(name_range, range)
        })
        .map(|dead| {
            let deletion_range = Range {
                start: index.to_position(&text, dead.deletion_range.start().into(), encoding),
                end: index.to_position(&text, dead.deletion_range.end().into(), encoding),
            };
            CodeActionOrCommand::CodeAction(CodeAction {
                title: format!(
                    "Remove unused {} '{}'",
                    apex_binder::kind_label(dead.kind, dead.visibility),
                    dead.name
                ),
                kind: Some(CodeActionKind::QUICKFIX),
                edit: Some(WorkspaceEdit {
                    changes: Some(HashMap::from([(
                        uri.clone(),
                        vec![TextEdit {
                            range: deletion_range,
                            new_text: String::new(),
                        }],
                    )])),
                    ..Default::default()
                }),
                ..Default::default()
            })
        })
        .collect()
}

/// One structural edit to a parameter/argument list -- shared by both a
/// declaration's `FormalParamList` and a call site's `ArgList`, since Apex
/// has no default/named/variadic arguments: a resolved, non-overloaded
/// call's `ArgList` always has exactly as many positional expressions, in
/// the same order, as the declaration has parameters, so the identical
/// index transform is always safe on either side.
#[derive(Clone, Copy)]
enum ParamOp {
    RotateLeft,
    RotateRight,
    RemoveAt(usize),
}

impl ParamOp {
    fn apply(self, items: &[String]) -> Vec<String> {
        match self {
            ParamOp::RotateLeft if !items.is_empty() => {
                let mut items = items.to_vec();
                items.rotate_left(1);
                items
            }
            ParamOp::RotateRight if !items.is_empty() => {
                let mut items = items.to_vec();
                items.rotate_right(1);
                items
            }
            ParamOp::RotateLeft | ParamOp::RotateRight => Vec::new(),
            ParamOp::RemoveAt(index) => items
                .iter()
                .enumerate()
                .filter(|&(i, _)| i != index)
                .map(|(_, s)| s.clone())
                .collect(),
        }
    }
}

/// `op` applied to `item_texts`, joined back into the flat, single-line
/// comma-separated form the rewritten list always takes (`", "`-joined;
/// empty when the result has zero items, correctly producing a bare `()`).
/// Deliberately reformats onto one line even when the original list was
/// written multi-line -- still correct Apex either way, just not
/// formatting-preserving, an accepted v1 simplification.
fn reordered_list_text(item_texts: &[String], op: ParamOp) -> String {
    op.apply(item_texts).join(", ")
}

/// `items`' own overall span (from the first item's start to the last
/// item's end -- never touching the list's surrounding `(`/`)`) and each
/// item's own trimmed source text, for `reordered_list_text` to
/// reconstruct. `None` for an empty list (a call site's own `ArgList`
/// should never actually be empty here -- the triggering method already
/// has at least one parameter -- but a defensive `Option` costs nothing).
/// Generic over `FormalParam`/`Expr` alike: both are real `AstNode`s over
/// this crate's own `ApexLanguage`, and both list shapes need the
/// identical span-and-texts computation.
fn list_span_and_texts<N: rowan::ast::AstNode<Language = apex_syntax::ApexLanguage>>(
    items: impl Iterator<Item = N>,
) -> Option<(TextRange, Vec<String>)> {
    let items: Vec<N> = items.collect();
    let span = TextRange::new(
        items.first()?.syntax().text_range().start(),
        items.last()?.syntax().text_range().end(),
    );
    let texts = items
        .iter()
        .map(|n| n.syntax().text().to_string().trim().to_string())
        .collect();
    Some((span, texts))
}

/// The multi-file `WorkspaceEdit` for applying `op` to `method_id`'s own
/// parameter list (a `Method` or a `Constructor` -- `SymbolKind` decides
/// which AST shapes to expect at both the declaration and each call
/// site): rewrites its declaration's `FormalParamList` plus every one of
/// `program.references_to`'s call sites. Each reference `ptr` is already
/// keyed at the whole call node -- a `MethodCallExpr` for a method
/// (`resolve::bind_method_call_expr`'s own `SyntaxPtr::new(self.file,
/// mc.syntax())`) or a `NewExpr` for a constructor
/// (`resolve::bind_new_expr`'s identical pattern) -- so casting it
/// directly gets the call's own `ArgList` with no further ancestor-
/// walking. `None` only if the declaration itself can't be re-derived
/// from `method_id` (shouldn't happen for a real `Method`/`Constructor`
/// symbol -- defensive, not expected). Mirrors `rename_edits`'s own
/// per-file `TextEdit` grouping (`per_file`/`LineIndex` cache, keyed by
/// `Url`) exactly, since both are genuinely project-wide, multi-file
/// edits.
fn parameter_op_workspace_edit(
    program: &BoundProgram,
    method_id: SymbolId,
    op: ParamOp,
    encoding: PositionEncoding,
) -> Option<WorkspaceEdit> {
    let decl_symbol = program.symbols.get(method_id);
    let is_constructor = decl_symbol.kind == SymbolKind::Constructor;
    let decl_root = program.syntax(decl_symbol.file);
    let decl_node = decl_symbol.ptr.to_node(&decl_root)?;
    let param_list = if is_constructor {
        ConstructorDecl::cast(decl_node)?.params()?
    } else {
        MethodDecl::cast(decl_node)?.params()?
    };
    let (decl_span, decl_texts) = list_span_and_texts(param_list.params())?;

    let mut sites: Vec<(FileId, TextRange, String)> =
        vec![(decl_symbol.file, decl_span, reordered_list_text(&decl_texts, op))];

    for ptr in program.references_to(method_id) {
        let root = program.syntax(ptr.file());
        let Some(node) = ptr.to_node(&root) else { continue };
        let args = if is_constructor {
            NewExpr::cast(node).and_then(|n| n.args())
        } else {
            MethodCallExpr::cast(node).and_then(|c| c.args())
        };
        let Some(args) = args else { continue };
        let Some((arg_span, arg_texts)) = list_span_and_texts(args.args()) else {
            continue;
        };
        sites.push((ptr.file(), arg_span, reordered_list_text(&arg_texts, op)));
    }

    let mut per_file: HashMap<FileId, (Url, String, LineIndex)> = HashMap::new();
    let mut changes: HashMap<Url, Vec<TextEdit>> = HashMap::new();
    for (site_file, site_range, new_text) in sites {
        if let std::collections::hash_map::Entry::Vacant(e) = per_file.entry(site_file) {
            let Some(uri) = Url::from_file_path(program.file_path(site_file)).ok() else {
                continue;
            };
            let text = program.syntax(site_file).text().to_string();
            let index = LineIndex::new(&text);
            e.insert((uri, text, index));
        }
        let Some((uri, text, index)) = per_file.get(&site_file) else {
            continue;
        };
        let start = index.to_position(text, site_range.start().into(), encoding);
        let end = index.to_position(text, site_range.end().into(), encoding);
        changes.entry(uri.clone()).or_default().push(TextEdit {
            range: Range { start, end },
            new_text,
        });
    }

    Some(WorkspaceEdit {
        changes: Some(changes),
        ..Default::default()
    })
}

/// The declared symbol (a `Method` or a `Constructor`) that owns
/// `param_list`, if any -- `param_list`'s immediate parent is either a
/// `MethodDecl` (whose own name is a separate `Name` node) or a
/// `ConstructorDecl` (whose "name" reuses the `Type` slot instead, since
/// Apex requires it to equal its class's own name -- see
/// `ConstructorDecl::type_ref`'s own doc comment; `collect::collect_constructor`
/// keys the `Symbol`'s `name_range` off that `Type`'s own last base-name
/// token for the identical reason). Either way, resolving through
/// `BoundProgram::symbol_at` off that name token's own start offset is
/// the same declaration-to-symbol lookup pattern used everywhere else in
/// this file.
fn declared_symbol_owning_param_list(
    program: &BoundProgram,
    file: FileId,
    param_list: &FormalParamList,
) -> Option<SymbolId> {
    let parent = param_list.syntax().parent()?;
    if let Some(method) = MethodDecl::cast(parent.clone()) {
        return program.symbol_at(file, method.name()?.ident_range().start());
    }
    if let Some(constructor) = ConstructorDecl::cast(parent) {
        let offset = constructor.type_ref()?.base_name_tokens().last()?.text_range().start();
        return program.symbol_at(file, offset);
    }
    None
}

/// `textDocument/codeAction`: "Rotate parameters left/right" and "Remove
/// parameter '<name>'" for a non-virtual, non-overloaded method or
/// constructor -- see `method_override_chain_reason`'s own doc comment
/// for the override-chain half of a *method*'s eligibility gate (moot for
/// a constructor: Apex constructors are never `virtual`/`override`, never
/// inherited, and interfaces can't declare one at all). The other half
/// -- no sibling declaration of the same name directly on the same class
/// -- applies to both, though it bites a constructor far more often:
/// every constructor of a class shares its class's own name, so this
/// refuses whenever a class has *more than one* constructor at all, not
/// just a genuine overload in the method sense. A method additionally
/// can't be declared directly on an `Interface`. All of this is
/// deliberately conservative for v1: a wider version handling virtual/
/// overloaded methods (and multi-constructor classes) safely is a real,
/// separate design problem (which declarations also need editing,
/// whether a call site's target overload is still unambiguous after the
/// edit), not attempted here.
///
/// Triggered only when `range`'s start lands inside a `FormalParam` of a
/// `MethodDecl`/`ConstructorDecl`'s own parameter list -- the same
/// token-then-ancestors lookup pattern `prepare_rename_range` already
/// uses. Every other case (no `FormalParam` there, the declaration fails
/// eligibility, zero parameters) quietly offers nothing, the same "don't
/// offer, don't explain" contract `dead_code_actions` already has --
/// unlike rename's `RenameRefusal`, there's no user-initiated single
/// target to explain a refusal *to*.
pub(crate) fn parameter_reorder_actions(
    program: &BoundProgram,
    file: FileId,
    range: Range,
    encoding: PositionEncoding,
) -> Vec<CodeActionOrCommand> {
    let root = program.syntax(file);
    let text = root.text().to_string();
    let index = LineIndex::new(&text);
    let Some(offset) = index.to_offset(&text, range.start, encoding) else {
        return Vec::new();
    };
    // A cursor sitting exactly on a token boundary (e.g. right after `(`,
    // with no whitespace before the parameter's own type -- a real
    // shape, not just a pathological input: `configure(String a)`'s
    // first parameter starts immediately after `(`) needs *both*
    // candidate tokens tried, not just one: `prepare_rename_range`'s own
    // "prefer the non-trivia side" rule picks whichever token is meant
    // for a *different* question ("what identifier is the cursor
    // renaming") and answers this one wrong here, since the non-trivia
    // side at a `(`/first-parameter boundary is the paren itself, not
    // the parameter. Trying the token that *starts* at this offset
    // first (what a cursor conventionally means to act on next) and
    // falling back to the one that *ends* here covers every real
    // boundary shape a parameter list can have.
    let candidates: Vec<apex_syntax::SyntaxToken> = match root.token_at_offset(offset.into()) {
        rowan::TokenAtOffset::None => return Vec::new(),
        rowan::TokenAtOffset::Single(t) => vec![t],
        rowan::TokenAtOffset::Between(left, right) => vec![right, left],
    };
    let Some(param) = candidates
        .iter()
        .find_map(|t| t.parent().and_then(|p| p.ancestors().find_map(FormalParam::cast)))
    else {
        return Vec::new();
    };
    let Some(param_list) = param.syntax().parent().and_then(FormalParamList::cast) else {
        return Vec::new();
    };
    let Some(method_id) = declared_symbol_owning_param_list(program, file, &param_list) else {
        return Vec::new();
    };

    let symbol = program.symbols.get(method_id);
    let Some(container) = symbol.container else {
        return Vec::new();
    };
    match symbol.kind {
        SymbolKind::Method => {
            if program.symbols.get(container).kind == SymbolKind::Interface {
                return Vec::new();
            }
            if method_override_chain_reason(program, method_id).is_some() {
                return Vec::new();
            }
            let has_sibling_overload = program.symbols.members_of(container).iter().any(|&m| {
                m != method_id
                    && program.symbols.get(m).kind == SymbolKind::Method
                    && program.symbols.get(m).name.eq_ignore_ascii_case(&symbol.name)
            });
            if has_sibling_overload {
                return Vec::new();
            }
        }
        SymbolKind::Constructor => {
            let has_sibling_constructor = program
                .symbols
                .members_of(container)
                .iter()
                .any(|&m| m != method_id && program.symbols.get(m).kind == SymbolKind::Constructor);
            if has_sibling_constructor {
                return Vec::new();
            }
        }
        _ => return Vec::new(),
    }

    let params: Vec<FormalParam> = param_list.params().collect();
    let Some(param_index) = params.iter().position(|p| p.syntax() == param.syntax()) else {
        return Vec::new();
    };

    let mut ops: Vec<(&'static str, ParamOp)> = Vec::new();
    if params.len() >= 2 {
        ops.push(("Rotate parameters left", ParamOp::RotateLeft));
        ops.push(("Rotate parameters right", ParamOp::RotateRight));
    }
    let remove_title = format!(
        "Remove parameter '{}'",
        params[param_index].name().and_then(|n| n.text()).unwrap_or_default()
    );

    ops.into_iter()
        .chain(std::iter::once((remove_title.as_str(), ParamOp::RemoveAt(param_index))))
        .filter_map(|(title, op)| {
            let edit = parameter_op_workspace_edit(program, method_id, op, encoding)?;
            Some(CodeActionOrCommand::CodeAction(CodeAction {
                title: title.to_string(),
                kind: Some(CodeActionKind::REFACTOR_REWRITE),
                edit: Some(edit),
                ..Default::default()
            }))
        })
        .collect()
}

/// `symbol`'s declaring type's name, for a `CallHierarchyItem`'s
/// `detail` field -- most clients show it alongside the bare method
/// name (`Foo.bar`'s detail is `Foo`), the same "which class this
/// belongs to" context `describe_symbol`'s hover signature line gives a
/// different way.
fn call_hierarchy_detail(program: &BoundProgram, symbol: &Symbol) -> Option<String> {
    let container = symbol.container?;
    Some(program.symbols.get(container).name.to_string())
}

/// One `SymbolId` (always `apex_binder::is_callable`, checked by every
/// caller of this) as a `CallHierarchyItem` -- shared by
/// `prepare_call_hierarchy` and both `incoming_calls`/`outgoing_calls`,
/// which each need to render a caller/callee back into the same shape
/// the client's own prepare request produced.
fn call_hierarchy_item(
    program: &BoundProgram,
    id: SymbolId,
    encoding: PositionEncoding,
) -> Option<CallHierarchyItem> {
    let symbol = program.symbols.get(id);
    let uri = Url::from_file_path(program.file_path(symbol.file)).ok()?;
    let root = program.syntax(symbol.file);
    let text = root.text().to_string();
    let index = LineIndex::new(&text);
    let to_range = |r: TextRange| Range {
        start: index.to_position(&text, r.start().into(), encoding),
        end: index.to_position(&text, r.end().into(), encoding),
    };
    Some(CallHierarchyItem {
        name: symbol.name.to_string(),
        kind: lsp_symbol_kind(symbol.kind),
        tags: None,
        detail: call_hierarchy_detail(program, symbol),
        uri,
        range: to_range(declaration_range(&root, symbol.ptr)),
        selection_range: to_range(symbol.name_range),
        data: None,
    })
}

/// `textDocument/prepareCallHierarchy`: the callable(s) at the cursor,
/// the same declaration-then-reference precedence `targets_at` already
/// gives `references`/`document_highlight`, filtered to
/// `apex_binder::is_callable` -- a `CallHierarchyItem` is always a
/// `Method`/`Constructor`, so a cursor on a field/local/type resolves to
/// nothing here even though `targets_at` itself would find something.
/// Every `Resolution::Candidates` entry is included, not just the
/// first, matching `references`/`document_highlight`'s own "don't
/// guess, show every candidate" convention for an ambiguous overload
/// call -- see `apex_binder::call_hierarchy`'s module doc comment for
/// exactly which calls stay ambiguous.
pub(crate) fn prepare_call_hierarchy(
    program: &BoundProgram,
    file: FileId,
    offset: TextSize,
    encoding: PositionEncoding,
) -> Vec<CallHierarchyItem> {
    targets_at(program, file, offset)
        .into_iter()
        .filter(|&id| apex_binder::is_callable(program.symbols.get(id).kind))
        .filter_map(|id| call_hierarchy_item(program, id, encoding))
        .collect()
}

/// `callHierarchy/incomingCalls`: every distinct caller of the callable
/// at `file`/`offset` (re-resolved from the client-echoed
/// `CallHierarchyItem`'s own `uri`/`selection_range.start`, via
/// `capabilities::resolve_position` in the caller of this function --
/// `CallHierarchyItem` carries no `SymbolId` of its own, and `data` is
/// left unused rather than round-tripping one, since re-resolving
/// against whichever `BoundProgram` snapshot is live at request time is
/// exactly the same "always resolve against the current bind" posture
/// every other capability here already takes). `None` when `offset`
/// doesn't land on a real callable's own declared name at all -- the
/// file was edited out from under a still-open hierarchy view, say.
pub(crate) fn incoming_calls(
    program: &BoundProgram,
    file: FileId,
    offset: TextSize,
    encoding: PositionEncoding,
) -> Option<Vec<CallHierarchyIncomingCall>> {
    let id = program.symbol_at(file, offset)?;
    if !apex_binder::is_callable(program.symbols.get(id).kind) {
        return None;
    }
    Some(
        apex_binder::incoming_calls(program, id)
            .into_iter()
            .filter_map(|call| {
                let from = call_hierarchy_item(program, call.from, encoding)?;
                // Every call site in `call.call_sites` lives in the same
                // file as `call.from` itself -- Apex has no partial
                // classes, so a caller's own body can never span files --
                // so one `LineIndex`, built once, covers the whole group.
                let caller_file = program.symbols.get(call.from).file;
                let root = program.syntax(caller_file);
                let text = root.text().to_string();
                let index = LineIndex::new(&text);
                let from_ranges = call
                    .call_sites
                    .into_iter()
                    .map(|ptr| {
                        let range = program.highlight_range(ptr);
                        Range {
                            start: index.to_position(&text, range.start().into(), encoding),
                            end: index.to_position(&text, range.end().into(), encoding),
                        }
                    })
                    .collect();
                Some(CallHierarchyIncomingCall { from, from_ranges })
            })
            .collect(),
    )
}

/// `callHierarchy/outgoingCalls`: every distinct callable the callable
/// at `file`/`offset` calls in its own body -- otherwise the mirror
/// image of `incoming_calls`, down to the re-resolution posture and the
/// same ambiguous-call fan-out.
pub(crate) fn outgoing_calls(
    program: &BoundProgram,
    file: FileId,
    offset: TextSize,
    encoding: PositionEncoding,
) -> Option<Vec<CallHierarchyOutgoingCall>> {
    let id = program.symbol_at(file, offset)?;
    if !apex_binder::is_callable(program.symbols.get(id).kind) {
        return None;
    }
    let caller_file = program.symbols.get(id).file;
    let root = program.syntax(caller_file);
    let text = root.text().to_string();
    let index = LineIndex::new(&text);
    Some(
        apex_binder::outgoing_calls(program, id)
            .into_iter()
            .filter_map(|call| {
                let to = call_hierarchy_item(program, call.to, encoding)?;
                let from_ranges = call
                    .call_sites
                    .into_iter()
                    .map(|ptr| {
                        let range = program.highlight_range(ptr);
                        Range {
                            start: index.to_position(&text, range.start().into(), encoding),
                            end: index.to_position(&text, range.end().into(), encoding),
                        }
                    })
                    .collect();
                Some(CallHierarchyOutgoingCall { to, from_ranges })
            })
            .collect(),
    )
}

/// `textDocument/signatureHelp`: which overload(s) the call the cursor
/// sits inside could resolve to, and which parameter position the
/// cursor is currently in. Deliberately does *not* start from
/// `BoundProgram::resolution_at` the way every other position-based
/// capability here does -- `resolution_at`'s own doc comment explains it
/// stops climbing at the *first* reference-kind ancestor, which for a
/// cursor sitting on an argument that's itself a name/field/call
/// (`foo(bar)`'s `bar`) would return `bar`'s own resolution, not the
/// enclosing call `foo(...)`'s -- exactly backwards for this feature.
/// Instead this climbs to the nearest enclosing `ArgList` first
/// (unambiguous: an `ArgList` only ever has one immediate parent, a
/// `MethodCallExpr`/`CallExpr`/`NewExpr`, and nested calls nest their
/// own `ArgList`s the same way their calls nest), then looks up *that*
/// node's own recorded `Resolution` directly via `resolution`.
///
/// The active parameter is the count of `Comma` *tokens* (not resolved
/// argument nodes) preceding the cursor, so a trailing comma with
/// nothing typed after it yet (`foo(1, |)`) still advances to the next
/// parameter slot -- `apex-parser`'s `arg_list` grammar
/// (`crates/apex-parser/src/grammar/expressions.rs`) completes the
/// `ArgList` node even with a missing closing paren or a dangling
/// trailing comma, so this needs no recovery logic of its own.
///
/// The candidate overload set is always recomputed fresh from
/// `container`/`name` (`SymbolTable::lookup_member`/`members_of`) rather
/// than read back from the stored `Resolution`: `narrow_by_overload`
/// already collapsed that down to a single winner (or an arity-narrowed
/// ambiguous set) by the time binding recorded it, so the *other*
/// overloads a real signature-help popup needs to show are never in the
/// stored value at all.
pub(crate) fn signature_help(
    program: &BoundProgram,
    file: FileId,
    offset: TextSize,
) -> Option<SignatureHelp> {
    let root = program.syntax(file);
    let token = match root.token_at_offset(offset) {
        rowan::TokenAtOffset::None => return None,
        rowan::TokenAtOffset::Single(t) => t,
        rowan::TokenAtOffset::Between(left, right) => {
            if left.kind().is_trivia() {
                right
            } else {
                left
            }
        }
    };

    let mut node = token.parent()?;
    let arg_list = loop {
        if let Some(arg_list) = ArgList::cast(node.clone()) {
            break arg_list;
        }
        node = node.parent()?;
    };
    let call_node = arg_list.syntax().parent()?;
    let resolution = program.resolution(SyntaxPtr::new(file, &call_node))?;

    let active_param = arg_list
        .syntax()
        .children_with_tokens()
        .filter_map(|el| el.into_token())
        .filter(|t| t.kind() == apex_syntax::SyntaxKind::Comma && t.text_range().end() <= offset)
        .count();
    let typed_args = arg_list.args().count();

    let (signatures, param_counts): (Vec<SignatureInformation>, Vec<usize>) = match resolution {
        Resolution::Resolved(id) => member_candidates(program, *id, file, offset)?,
        Resolution::Candidates(ids) => member_candidates(program, *ids.first()?, file, offset)?,
        Resolution::StdlibMember(r) => stdlib_candidates(program, r)?,
        _ => return None,
    };

    let active_signature = pick_active_signature(&param_counts, active_param, typed_args);
    let active_parameter = param_counts
        .get(active_signature)
        .map_or(active_param, |&n| active_param.min(n.saturating_sub(1)));

    Some(SignatureHelp {
        signatures,
        active_signature: Some(active_signature as u32),
        active_parameter: Some(active_parameter as u32),
    })
}

/// Every visible same-named `Method`/`Constructor` overload sharing
/// `first_id`'s own `container`/`name` -- the full candidate set
/// `signature_help` needs, reconstructed the same way
/// `resolve::bind_method_call_expr`/`bind_new_expr` originally built it
/// before `narrow_by_overload` collapsed it down to `first_id` alone.
/// Visibility is only checked when `file`/`offset` actually has an
/// enclosing method/constructor to check it *from* (a call in a field/
/// property initializer doesn't) -- unfiltered rather than
/// wrongly-filtered in that case, matching this codebase's general
/// "don't guess, but don't silently drop it either" posture.
fn member_candidates(
    program: &BoundProgram,
    first_id: SymbolId,
    file: FileId,
    offset: TextSize,
) -> Option<(Vec<SignatureInformation>, Vec<usize>)> {
    let symbol = program.symbols.get(first_id);
    let container = symbol.container?;
    let enclosing_type = program
        .enclosing_callable(file, offset)
        .and_then(|m| program.symbols.get(m).container);
    let visible = |&id: &SymbolId| {
        enclosing_type.is_none() || program.symbols.is_visible_from(id, enclosing_type)
    };
    let candidates: Vec<SymbolId> = match symbol.kind {
        SymbolKind::Method => program
            .symbols
            .lookup_member(container, &symbol.name)
            .into_iter()
            .filter(|&id| program.symbols.get(id).kind == SymbolKind::Method)
            .filter(visible)
            .collect(),
        SymbolKind::Constructor => program
            .symbols
            .members_of(container)
            .iter()
            .copied()
            .filter(|&id| program.symbols.get(id).kind == SymbolKind::Constructor)
            .filter(visible)
            .collect(),
        _ => return None,
    };
    if candidates.is_empty() {
        return None;
    }
    let param_counts = candidates.iter().map(|&id| program.symbols.params(id).len()).collect();
    let signatures = candidates
        .iter()
        .map(|&id| method_signature_information(program, id))
        .collect();
    Some((signatures, param_counts))
}

/// The stdlib counterpart to [`member_candidates`]: every overload of
/// `r.member` on `r.class_name` from `program.stdlib`, rendered straight
/// from the bundled scraped schema (no `SymbolId`/declaration backs a
/// stdlib member at all, the same reason `describe_stdlib_member` looks
/// it up the same way).
fn stdlib_candidates(
    program: &BoundProgram,
    r: &StdlibMemberRef,
) -> Option<(Vec<SignatureInformation>, Vec<usize>)> {
    let member = r.member.as_deref()?;
    let overloads: Vec<_> = program.stdlib.methods(&r.class_name, member).collect();
    if overloads.is_empty() {
        return None;
    }
    let param_counts = overloads.iter().map(|m| m.params.len()).collect();
    let signatures = overloads
        .iter()
        .map(|m| {
            let param_labels: Vec<String> = m.params.iter().map(stdlib_param_label).collect();
            let ret = m.return_type.as_deref().unwrap_or("void");
            SignatureInformation {
                label: format!("{ret} {}({})", m.name, param_labels.join(", ")),
                documentation: m
                    .description
                    .as_ref()
                    .map(|d| Documentation::String(d.to_string())),
                parameters: Some(parameter_information(param_labels)),
                active_parameter: None,
            }
        })
        .collect();
    Some((signatures, param_counts))
}

fn method_signature_information(program: &BoundProgram, id: SymbolId) -> SignatureInformation {
    let symbol = program.symbols.get(id);
    let param_labels: Vec<String> = program
        .symbols
        .params(id)
        .iter()
        .map(|&param_id| {
            let param = program.symbols.get(param_id);
            match &param.type_name {
                Some(t) => format!("{t} {}", param.name),
                None => param.name.to_string(),
            }
        })
        .collect();
    let label = if symbol.kind == SymbolKind::Method {
        format!(
            "{} {}({})",
            symbol.type_name.as_deref().unwrap_or("void"),
            symbol.name,
            param_labels.join(", ")
        )
    } else {
        format!("{}({})", symbol.name, param_labels.join(", "))
    };
    SignatureInformation {
        label,
        documentation: None,
        parameters: Some(parameter_information(param_labels)),
        active_parameter: None,
    }
}

fn parameter_information(labels: Vec<String>) -> Vec<ParameterInformation> {
    labels
        .into_iter()
        .map(|label| ParameterInformation {
            label: ParameterLabel::Simple(label),
            documentation: None,
        })
        .collect()
}

/// Picks which overload should show as "active" given how many parameter
/// slots the cursor implies (`active_param + 1`, since a cursor sitting
/// in the Nth slot means at least N+1 parameters) and how many arguments
/// are actually typed so far (`typed_args`, which can be smaller than
/// `active_param + 1` when a trailing comma has nothing after it yet).
/// Prefers an exact match on `typed_args` (the call exactly as it stands
/// right now), then falls back to the first overload with a parameter
/// slot at `active_param` at all, then just the first overload -- never
/// `None` once `param_counts` is non-empty.
fn pick_active_signature(param_counts: &[usize], active_param: usize, typed_args: usize) -> usize {
    param_counts
        .iter()
        .position(|&n| n == typed_args)
        .or_else(|| param_counts.iter().position(|&n| n > active_param))
        .unwrap_or(0)
}

/// `textDocument/inlayHint`: a `paramName:` label before each call
/// argument, mirroring rust-analyzer's/clangd's default inlay-hint
/// behavior for call sites. Built from `BoundProgram::call_sites_in_range`
/// (the same on-demand, no-precomputed-index primitive
/// `call_hierarchy::outgoing_calls` already uses), so this costs nothing
/// beyond whatever range the client actually asked to render.
///
/// Only emitted for an unambiguous call -- a project call resolved to
/// exactly one `Resolution::Resolved` `Method`/`Constructor`, or a
/// stdlib call (`Resolution::StdlibMember`) whose overload set narrows
/// to exactly one candidate by the number of arguments actually typed.
/// An ambiguous `Resolution::Candidates` call, or a stdlib call whose
/// arity doesn't narrow to one overload, is skipped rather than
/// guessing: unlike a hover tooltip, an inlay hint is baked directly
/// into the editor's rendering of the line, so a wrong guess would be
/// far more visible/misleading than an honest absence. A stdlib
/// parameter the scraper couldn't extract a name for (`StdlibParam::name`
/// is `None` -- see its own doc comment) is skipped individually rather
/// than dropping the whole call's hints over one missing name.
///
/// Suppressed when the argument is itself a bare identifier that
/// already spells the parameter's own name (`foo(accountId)` for a
/// `foo(Id accountId)` parameter) -- the hint would be pure noise
/// repeating text already on the line, the same suppression
/// rust-analyzer/clangd both apply by default.
/// `textDocument/completion`: every candidate for the resolved context
/// (`apex_binder::complete_at`), turned into `CompletionItem`s. No
/// server-side filtering by whatever prefix has already been typed --
/// the full candidate set for the resolved context is always returned
/// (`is_incomplete: false`); each item carries a `text_edit` covering
/// `replace_range` so accepting one overwrites rather than duplicates
/// any already-typed partial text, and the client's own fuzzy matcher
/// narrows the visible list further as the user keeps typing (standard
/// LSP architecture, and much simpler than reimplementing fuzzy
/// matching here).
pub(crate) fn completion(
    program: &BoundProgram,
    file: FileId,
    offset: TextSize,
    encoding: PositionEncoding,
) -> Option<CompletionResponse> {
    let ctx = apex_binder::complete_at(program, file, offset)?;
    let text = program.syntax(file).text().to_string();
    let index = LineIndex::new(&text);
    let range = Range {
        start: index.to_position(&text, ctx.replace_range.start().into(), encoding),
        end: index.to_position(&text, ctx.replace_range.end().into(), encoding),
    };

    let items = ctx
        .candidates
        .into_iter()
        .map(|c| {
            let detail = completion_detail(program, &c);
            let sort_text = format!("{}{}", sort_tier(&c), c.label);
            let label = c.label.to_string();
            CompletionItem {
                label: label.clone(),
                kind: Some(lsp_completion_kind(c.kind)),
                detail,
                sort_text: Some(sort_text),
                text_edit: Some(CompletionTextEdit::Edit(TextEdit {
                    range,
                    new_text: label,
                })),
                insert_text_format: Some(InsertTextFormat::PLAIN_TEXT),
                ..Default::default()
            }
        })
        .collect();

    Some(CompletionResponse::List(CompletionList {
        is_incomplete: false,
        items,
    }))
}

/// A single leading sort-tier digit, so the client's default ascending
/// sort (`sort_text`, then falling back to `label`) groups locals first,
/// then direct members/project-wide types (structurally indistinguishable
/// from each other on `CompletionCandidate` -- a project-wide type is
/// exactly as directly relevant as a direct member when completing a
/// bare identifier, so sharing a tier is a reasonable simplification, not
/// an oversight), then inherited members, then stdlib, then SObject
/// fields, then keywords last.
fn sort_tier(c: &CompletionCandidate) -> &'static str {
    match c.kind {
        CompletionCandidateKind::Local | CompletionCandidateKind::Parameter => "0",
        CompletionCandidateKind::StdlibClass
        | CompletionCandidateKind::StdlibMethod
        | CompletionCandidateKind::StdlibProperty => "3",
        CompletionCandidateKind::SObjectField => "4",
        CompletionCandidateKind::Keyword => "5",
        _ => {
            if c.is_inherited {
                "2"
            } else {
                "1"
            }
        }
    }
}

/// `CompletionItem::detail`: a plain one-line signature, reusing this
/// module's own existing hover/signature-help formatters end-to-end
/// (`symbol_signature` for a project-local candidate, the same stdlib/
/// schema lookups `describe_stdlib_member`/`schema_location` already do
/// for the other two) rather than a second formatter living in
/// `apex_binder`'s protocol-agnostic `completion` module. Deliberately
/// shorter than `describe_symbol`'s hover text: no Markdown fence, no doc
/// comment/description -- `detail` is a single-line list annotation, not
/// a hover popup.
fn completion_detail(program: &BoundProgram, c: &CompletionCandidate) -> Option<String> {
    if let Some(id) = c.symbol {
        return Some(symbol_signature(program, id));
    }
    if let Some(class_name) = &c.stdlib_class {
        if let Some(prop) = program.stdlib.property(class_name, &c.label) {
            let modifier = if prop.is_static { "static " } else { "" };
            let ty = prop.type_name.as_deref().unwrap_or("Object");
            return Some(format!("public {modifier}{ty} {}", prop.name));
        }
        let m = program.stdlib.method(class_name, &c.label)?;
        let modifier = if m.is_static { "static " } else { "" };
        let ret = m.return_type.as_deref().unwrap_or("void");
        let params = m.params.iter().map(stdlib_param_label).collect::<Vec<_>>().join(", ");
        return Some(format!("public {modifier}{ret} {}({params})", m.name));
    }
    if let Some((object, field)) = &c.sobject_field {
        let ty = program
            .schema
            .field(object, field)
            .and_then(|f| f.field_type.as_deref())
            .unwrap_or("Object");
        return Some(format!("{ty} {field}"));
    }
    None
}

/// `apex_binder::CompletionCandidateKind` -> the closest `lsp_types::CompletionItemKind`.
fn lsp_completion_kind(kind: CompletionCandidateKind) -> CompletionItemKind {
    match kind {
        CompletionCandidateKind::Local
        | CompletionCandidateKind::Parameter => CompletionItemKind::VARIABLE,
        CompletionCandidateKind::Field | CompletionCandidateKind::SObjectField => {
            CompletionItemKind::FIELD
        }
        CompletionCandidateKind::Property | CompletionCandidateKind::StdlibProperty => {
            CompletionItemKind::PROPERTY
        }
        CompletionCandidateKind::Method | CompletionCandidateKind::StdlibMethod => {
            CompletionItemKind::METHOD
        }
        CompletionCandidateKind::Constructor => CompletionItemKind::CONSTRUCTOR,
        CompletionCandidateKind::EnumConstant => CompletionItemKind::ENUM_MEMBER,
        CompletionCandidateKind::Class | CompletionCandidateKind::StdlibClass => {
            CompletionItemKind::CLASS
        }
        CompletionCandidateKind::Interface => CompletionItemKind::INTERFACE,
        CompletionCandidateKind::Enum => CompletionItemKind::ENUM,
        CompletionCandidateKind::Keyword => CompletionItemKind::KEYWORD,
    }
}

pub(crate) fn inlay_hints(
    program: &BoundProgram,
    uri: &Url,
    range: Range,
    encoding: PositionEncoding,
) -> Option<Vec<InlayHint>> {
    let (file, start) = resolve_position(program, uri, range.start, encoding)?;
    let (_, end) = resolve_position(program, uri, range.end, encoding)?;
    let byte_range = TextRange::new(start, end);

    let root = program.syntax(file);
    let text = root.text().to_string();
    let index = LineIndex::new(&text);
    let make_hint = |arg: &Expr, name: &str| -> Option<InlayHint> {
        if argument_repeats_param_name(arg, name) {
            return None;
        }
        let pos = arg.syntax().text_range().start();
        Some(InlayHint {
            position: index.to_position(&text, pos.into(), encoding),
            label: InlayHintLabel::String(format!("{name}:")),
            kind: Some(InlayHintKind::PARAMETER),
            text_edits: None,
            tooltip: None,
            padding_left: None,
            padding_right: Some(true),
            data: None,
        })
    };

    Some(
        program
            .call_sites_in_range(file, byte_range)
            .into_iter()
            .filter_map(|(ptr, resolution)| {
                let node = ptr.to_node(&root)?;
                let args: Vec<Expr> = call_arg_list(&node)?.args().collect();
                match resolution {
                    Resolution::Resolved(id) => {
                        let symbol = program.symbols.get(id);
                        if !matches!(symbol.kind, SymbolKind::Method | SymbolKind::Constructor) {
                            return None;
                        }
                        let params = program.symbols.params(id);
                        Some(
                            params
                                .into_iter()
                                .zip(args)
                                .filter_map(|(param_id, arg)| {
                                    make_hint(&arg, &program.symbols.get(param_id).name)
                                })
                                .collect::<Vec<_>>(),
                        )
                    }
                    Resolution::StdlibMember(r) => {
                        let member = r.member.as_deref()?;
                        let overloads: Vec<_> = program.stdlib.methods(&r.class_name, member).collect();
                        let narrowed: Vec<_> =
                            overloads.iter().filter(|m| m.params.len() == args.len()).collect();
                        let [winner] = narrowed.as_slice() else {
                            return None;
                        };
                        Some(
                            winner
                                .params
                                .iter()
                                .zip(args)
                                .filter_map(|(param, arg)| make_hint(&arg, param.name.as_deref()?))
                                .collect::<Vec<_>>(),
                        )
                    }
                    _ => None,
                }
            })
            .flatten()
            .collect(),
    )
}

/// The `ArgList` of a call node already known to be one of the three
/// kinds `BoundProgram::call_sites_in_range` ever returns
/// (`MethodCallExpr`/`CallExpr`/`NewExpr`) -- tries each in turn since
/// there's no common supertype to cast through directly.
fn call_arg_list(node: &apex_syntax::SyntaxNode) -> Option<ArgList> {
    MethodCallExpr::cast(node.clone())
        .and_then(|m| m.args())
        .or_else(|| CallExpr::cast(node.clone()).and_then(|c| c.args()))
        .or_else(|| NewExpr::cast(node.clone()).and_then(|n| n.args()))
}

fn argument_repeats_param_name(arg: &Expr, param_name: &str) -> bool {
    NameExpr::cast(arg.syntax().clone())
        .and_then(|n| n.name_token())
        .is_some_and(|tok| tok.text().eq_ignore_ascii_case(param_name))
}

