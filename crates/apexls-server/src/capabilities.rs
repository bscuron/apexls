//! Shared plumbing for position-based capabilities (`textDocument/hover`,
//! `textDocument/definition`, and whatever else `BACKLOG.md` §3 adds
//! next): resolving an LSP `(Url, Position)` down to the coordinates
//! `apex_binder::BoundProgram`'s own API understands, and the reverse --
//! turning a resolved `SymbolId` back into an LSP `Location`.

use crate::line_index::{LineIndex, PositionEncoding};
use apex_binder::{
    BoundProgram, CompletionCandidate, CompletionCandidateKind, FileId, Resolution,
    SchemaObjectRef, StdlibMemberRef, Symbol, SymbolId, SymbolKind, SyntaxPtr, Visibility,
};
use apex_syntax::ast::decl::{
    ClassDecl, ConstructorDecl, EnumDecl, FieldDecl, HasDocComment, InterfaceDecl, MethodDecl,
    PropertyDecl, TriggerUnit,
};
use apex_syntax::ast::expr::{ArgList, CallExpr, Expr, MethodCallExpr, NameExpr, NewExpr};
use lsp_types::{
    CallHierarchyIncomingCall, CallHierarchyItem, CallHierarchyOutgoingCall, CodeAction,
    CodeActionKind, CodeActionOrCommand, CompletionItem, CompletionItemKind, CompletionList,
    CompletionResponse, CompletionTextEdit, Diagnostic, DiagnosticSeverity, DiagnosticTag,
    Documentation, DocumentHighlight, DocumentSymbol, FoldingRange, InlayHint, InlayHintKind,
    InlayHintLabel, InsertTextFormat, Location, ParameterInformation, ParameterLabel, Position,
    Range, SelectionRange, SignatureHelp, SignatureInformation, SymbolInformation,
    SymbolKind as LspSymbolKind, TextEdit, Url, WorkspaceEdit,
};
use rowan::ast::AstNode;
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

/// A `Method` is renameable only when it can't be part of an override
/// chain this feature doesn't cascade across (a distinct, unsolved problem
/// from overload-call ambiguity): not itself marked `override`, not
/// implementing an interface/base-class method of the same name and arity
/// (Apex requires no `override` keyword for that case), and not itself
/// overridden by any subclass. The last check scans every project symbol
/// -- no index answers "which methods override this one" the way
/// `SymbolTable`'s other lookups are O(1), but that's fine here: a rename
/// is a rare, user-initiated action, not a per-keystroke path the rest of
/// this codebase optimizes for.
fn check_method_eligible(program: &BoundProgram, id: SymbolId) -> Result<(), RenameRefusal> {
    let symbol = program.symbols.get(id);
    if symbol.modifiers.is_override {
        return Err(RenameRefusal::OverrideChain(
            "it overrides a base class method",
        ));
    }
    let Some(container) = symbol.container else {
        return Ok(());
    };
    let arity = program.symbols.params(id).len();

    for &ancestor in program.symbols.inherited_chain(container) {
        for candidate in program.symbols.lookup_member(ancestor, &symbol.name) {
            if candidate != id && program.symbols.params(candidate).len() == arity {
                return Err(RenameRefusal::OverrideChain(
                    "it implements an interface or base-class method of the same name",
                ));
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
        return Err(RenameRefusal::OverrideChain("it's overridden by a subclass"));
    }
    Ok(())
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

