//! Shared plumbing for position-based capabilities (`textDocument/hover`,
//! `textDocument/definition`, and whatever else `BACKLOG.md` §3 adds
//! next): resolving an LSP `(Url, Position)` down to the coordinates
//! `apex_binder::BoundProgram`'s own API understands, and the reverse --
//! turning a resolved `SymbolId` back into an LSP `Location`.

use crate::line_index::{LineIndex, PositionEncoding};
use apex_binder::{
    BoundProgram, FileId, Resolution, SchemaObjectRef, Symbol, SymbolId, SymbolKind, SyntaxPtr,
    Visibility,
};
use apex_syntax::ast::decl::{
    ClassDecl, ConstructorDecl, EnumDecl, FieldDecl, HasDocComment, InterfaceDecl, MethodDecl,
    PropertyDecl, TriggerUnit,
};
use lsp_types::{
    DocumentHighlight, DocumentSymbol, FoldingRange, Location, Position, Range, SelectionRange,
    SymbolInformation, SymbolKind as LspSymbolKind, Url,
};
use rowan::{TextRange, TextSize};

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
/// so this keys off `ptr.file()`/`ptr.range()` directly instead.
pub(crate) fn ptr_location(
    program: &BoundProgram,
    ptr: SyntaxPtr,
    encoding: PositionEncoding,
) -> Option<Location> {
    let uri = Url::from_file_path(program.file_path(ptr.file())).ok()?;
    let text = program.syntax(ptr.file()).text().to_string();
    let index = LineIndex::new(&text);
    let range = ptr.range();
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
        Some(field) => &program.schema.field(&r.object, field)?.source_path,
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

    let mut out = format!("```apex\n{sig}\n```");
    if let Some(doc) = doc_comment_for(program, id) {
        if !doc.is_empty() {
            out.push_str("\n\n");
            out.push_str(&doc);
        }
    }
    out
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

fn doc_comment_for(program: &BoundProgram, id: SymbolId) -> Option<String> {
    use rowan::ast::AstNode;

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
