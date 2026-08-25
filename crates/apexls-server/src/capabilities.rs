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
    ClassDecl, ConstructorDecl, EnumDecl, FieldDecl, HasDocComment, HasModifiers, InterfaceDecl,
    MethodDecl, PropertyDecl, TriggerUnit, VarDeclarator,
};
use apex_syntax::ast::stmt::LocalVarDeclStmt;
use lsp_types::{
    CodeAction, CodeActionKind, CodeActionOrCommand, Diagnostic, DiagnosticSeverity,
    DiagnosticTag, DocumentHighlight, DocumentSymbol, FoldingRange, Location, Position, Range,
    SelectionRange, SymbolInformation, SymbolKind as LspSymbolKind, TextEdit, Url, WorkspaceEdit,
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

/// Which declaration kinds `dead_symbols_in_file` ever considers, and
/// under what visibility. Every reflection/platform-invocable annotation
/// that matters (`@InvocableMethod`, `@AuraEnabled`, `@RestResource`,
/// `@RemoteAction`, an interface method) requires `public`/`global`
/// visibility in real Apex -- interface methods can't even be `private`
/// -- so restricting `Method`/`Field`/`Property` to `Private` genuinely
/// excludes that whole platform-reflection channel, with no annotation
/// modeling needed to do it. `Constructor` is deliberately excluded even
/// though it shares `Method`'s shape: a private zero-arg constructor is
/// the standard idiom for blocking external instantiation of a static
/// utility class -- it's *supposed* to have zero call sites, and Apex
/// synthesizes an implicit public no-arg constructor when none is
/// declared, so deleting it would silently make the class instantiable
/// again. `ForEachVar`/`CatchVar`/`SwitchBindingVar` are excluded too:
/// removing an unused loop/catch/switch-binding variable would break the
/// surrounding syntax, so there's no safe quick-fix to offer, and a
/// diagnostic with no available fix isn't worth the noise.
fn is_dead_code_candidate_kind(symbol: &Symbol) -> bool {
    match symbol.kind {
        SymbolKind::Method | SymbolKind::Field | SymbolKind::Property => {
            symbol.modifiers.visibility == Visibility::Private
        }
        SymbolKind::LocalVar => true,
        _ => false,
    }
}

/// True for a private `Method` the Apex test-execution engine invokes
/// directly with zero textual call sites: `@isTest`/`@TestSetup`, or the
/// legacy `testMethod` modifier keyword (already modeled via
/// `ModifierSet::is_testmethod`). Must be checked before trusting a
/// `Method`'s zero-`references_to_in_file` count as proof of deadness --
/// unlike `@InvocableMethod`/`@AuraEnabled`/etc. (excluded structurally,
/// since those require `public`/`global` visibility and so never reach
/// this check at all), Apex test methods are routinely `private` and are
/// still invoked directly by the platform's test runner, never by other
/// Apex.
fn is_platform_invoked_test_method(program: &BoundProgram, symbol: &Symbol) -> bool {
    if symbol.kind != SymbolKind::Method {
        return false;
    }
    if symbol.modifiers.is_testmethod {
        return true;
    }
    let root = program.syntax(symbol.file);
    let Some(node) = symbol.ptr.to_node(&root) else {
        return false;
    };
    let Some(method) = MethodDecl::cast(node) else {
        return false;
    };
    method.annotations().any(|a| {
        a.name().is_some_and(|tok| {
            let text = tok.text();
            text.eq_ignore_ascii_case("isTest") || text.eq_ignore_ascii_case("testSetup")
        })
    })
}

/// One provably-dead declaration in a file: everything both
/// `dead_code_diagnostics` and `dead_code_actions` need, computed once
/// and shared between them.
struct DeadSymbol {
    kind: SymbolKind,
    name: String,
    name_range: TextRange,
    deletion_range: TextRange,
}

/// Every declaration in `file` this binder can *prove* is dead: a
/// private method/field/property, or a plain local variable, with zero
/// references anywhere `references_to_in_file` can see, and not one of
/// the platform-invoked exceptions above. Uses the file-scoped
/// `references_to_in_file`, not the project-wide `references_to`, quite
/// deliberately -- every symbol reaching this filter is either `Private`
/// (genuinely file-scoped by Apex's own visibility rules: one top-level
/// type per file, private members reachable only from that file's own
/// outer/nested classes) or a `LocalVar` (scope-bounded within its own
/// file by construction), so the file-scoped lookup isn't just an
/// optimization here, it's the *correct* one -- a project-wide scan
/// would cost more for zero additional correctness.
fn dead_symbols_in_file(program: &BoundProgram, file: FileId) -> Vec<DeadSymbol> {
    program
        .symbols
        .iter()
        .filter(|(_, s)| s.file == file)
        .filter(|(_, s)| is_dead_code_candidate_kind(s))
        .filter(|(_, s)| !is_platform_invoked_test_method(program, s))
        .filter(|(id, _)| program.references_to_in_file(file, *id).next().is_none())
        .filter_map(|(_, s)| {
            compute_deletion_range(program, s).map(|deletion_range| DeadSymbol {
                kind: s.kind,
                name: s.name.to_string(),
                name_range: s.name_range,
                deletion_range,
            })
        })
        .collect()
}

/// The exact text range to delete to remove `symbol` cleanly. Not simply
/// `symbol.ptr`'s range: that only covers the whole declaration for
/// `Method`/`Property` (see `Symbol::ptr`'s own doc comment) -- for
/// `Field` it's just the one `VarDeclarator`, and for `LocalVar` just the
/// `Name` token, since both `FieldDecl`/`LocalVarDeclStmt` support
/// multiple comma-separated declarators (`private Integer x, y;`) and
/// `Symbol` is one-per-declarator. "Delete this one" therefore means
/// either the whole declaration (it's the only declarator -- and, for a
/// `Field`, this naturally includes any leading doc comment/modifiers,
/// since `HasDocComment::doc_comment_token` finds the doc comment by
/// walking the very node whose range this returns) or just this
/// declarator plus its neighboring comma (siblings exist, so the shared
/// doc comment/modifiers must stay untouched -- they still document the
/// remaining declarators).
///
/// The whole-declaration case goes through `line_aligned_deletion_range`
/// rather than trusting the node's own raw boundaries directly: this
/// parser's trivia attachment at a statement/declaration's edges turned
/// out not to be trustworthy for this purpose empirically (verified via
/// `dead_code_tests`' splice checks) -- a `LocalVarDeclStmt`'s range, for
/// one, excludes its own leading indentation but can still include
/// trailing whitespace past its own line. Rather than chase that per-kind,
/// `line_aligned_deletion_range` sidesteps it entirely by computing
/// purely from the raw source text once a real-content anchor is found.
fn compute_deletion_range(program: &BoundProgram, symbol: &Symbol) -> Option<TextRange> {
    let root = program.syntax(symbol.file);
    let node = symbol.ptr.to_node(&root)?;
    let text = root.text().to_string();
    match symbol.kind {
        SymbolKind::Method | SymbolKind::Property => {
            Some(line_aligned_deletion_range(&text, node.text_range()))
        }
        SymbolKind::Field => {
            let declarator = VarDeclarator::cast(node)?;
            let field_decl = FieldDecl::cast(declarator.syntax().parent()?)?;
            let siblings: Vec<VarDeclarator> = field_decl.declarators().collect();
            if siblings.len() == 1 {
                Some(line_aligned_deletion_range(&text, field_decl.syntax().text_range()))
            } else {
                deletion_range_for_declarator(&declarator, &siblings)
            }
        }
        SymbolKind::LocalVar => {
            let declarator = VarDeclarator::cast(node.parent()?)?;
            let stmt = LocalVarDeclStmt::cast(declarator.syntax().parent()?)?;
            let siblings: Vec<VarDeclarator> = stmt.declarators().collect();
            if siblings.len() == 1 {
                Some(line_aligned_deletion_range(&text, stmt.syntax().text_range()))
            } else {
                deletion_range_for_declarator(&declarator, &siblings)
            }
        }
        _ => None,
    }
}

/// Shared by the `Field`/`LocalVar` arms above, for the case that has
/// *other* declarators to leave untouched: `declarator`'s own range plus
/// whichever neighboring comma separates it from the rest of `siblings`
/// (every declarator of the same `FieldDecl`/`LocalVarDeclStmt`, in
/// source order) -- the *following* comma if this isn't the last
/// declarator, otherwise the *preceding* one -- so deleting the middle
/// of `x, y, z` leaves `x, z`, not `x, , z` or a trailing `x, y,`. Not
/// line-aligned: a non-last declarator never owns its own line, so the
/// line-based reasoning `line_aligned_deletion_range` uses doesn't apply
/// here, only mid-line comma-splicing does.
fn deletion_range_for_declarator(
    declarator: &VarDeclarator,
    siblings: &[VarDeclarator],
) -> Option<TextRange> {
    let target = declarator.syntax().text_range();
    let index = siblings.iter().position(|d| d.syntax().text_range() == target)?;
    if index + 1 < siblings.len() {
        Some(TextRange::new(
            siblings[index].syntax().text_range().start(),
            siblings[index + 1].syntax().text_range().start(),
        ))
    } else {
        Some(TextRange::new(
            siblings[index - 1].syntax().text_range().end(),
            siblings[index].syntax().text_range().end(),
        ))
    }
}

/// Computes a deletion range that removes exactly the whole source
/// line(s) `range`'s *real* content occupies -- neither a leftover blank
/// line nor an orphaned indent -- without trusting `range`'s own
/// leading/trailing edges to already be whitespace-free or line-aligned
/// (this parser's trivia attachment varies by node kind: a statement's
/// range can exclude its own leading indentation yet still include
/// trailing whitespace reaching into the next line, as `compute_deletion_range`'s
/// doc comment explains). Three steps, all directly on the raw source
/// text rather than the syntax tree: (1) trim `range` down to its real
/// (non-whitespace) content on both edges, discarding whatever
/// whitespace it happened to include; (2) if only spaces/tabs sit
/// between that content's start and the start of its own line, extend
/// the start back to the line's start, so the declaration's own
/// indentation goes with it; (3) extend the end forward past exactly one
/// trailing line terminator (and any same-line trailing whitespace
/// before it), so the line itself -- not just its content -- disappears.
/// Deliberately never reaches into the *previous* line's own trailing
/// newline (that would merge the previous line into whatever now follows
/// instead of just closing this line's own gap).
fn line_aligned_deletion_range(text: &str, range: TextRange) -> TextRange {
    let bytes = text.as_bytes();
    let mut start = usize::from(range.start());
    let mut end = usize::from(range.end());
    while start < end && bytes[start].is_ascii_whitespace() {
        start += 1;
    }
    while end > start && bytes[end - 1].is_ascii_whitespace() {
        end -= 1;
    }

    let line_start = text[..start].rfind('\n').map(|i| i + 1).unwrap_or(0);
    if text.as_bytes()[line_start..start]
        .iter()
        .all(|&b| b == b' ' || b == b'\t')
    {
        start = line_start;
    }

    let mut i = end;
    while i < bytes.len() && (bytes[i] == b' ' || bytes[i] == b'\t') {
        i += 1;
    }
    if i < bytes.len() && bytes[i] == b'\r' {
        i += 1;
    }
    if i < bytes.len() && bytes[i] == b'\n' {
        i += 1;
    }
    TextRange::new(TextSize::from(start as u32), TextSize::from(i as u32))
}

fn kind_label(kind: SymbolKind) -> &'static str {
    match kind {
        SymbolKind::Method => "private method",
        SymbolKind::Field => "private field",
        SymbolKind::Property => "private property",
        SymbolKind::LocalVar => "local variable",
        _ => "declaration",
    }
}

/// `textDocument/publishDiagnostics`: one `WARNING`-severity diagnostic
/// per symbol `dead_symbols_in_file` proves is dead, tagged
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
    dead_symbols_in_file(program, file)
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
                message: format!("{} '{}' is never used", kind_label(dead.kind), dead.name),
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
    dead_symbols_in_file(program, file)
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
                title: format!("Remove unused {} '{}'", kind_label(dead.kind), dead.name),
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

#[cfg(test)]
mod dead_code_tests {
    use super::*;

    /// Writes `src` as `Foo.cls` under a fresh, uniquely-named temp
    /// directory (`test_name` keeps directories from colliding across
    /// tests running in parallel in the same process -- matching
    /// `crates/apex-binder/tests/*.rs`'s own `write_fixture_dir`
    /// convention).
    fn write_fixture(test_name: &str, src: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "apexls-server-capabilities-dead-code-{test_name}-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("Foo.cls"), src).unwrap();
        dir
    }

    fn dead_symbols(test_name: &str, src: &str) -> (BoundProgram, FileId, Vec<DeadSymbol>) {
        let dir = write_fixture(test_name, src);
        let program = BoundProgram::from_files(&dir);
        let file = program.file_id(&dir.join("Foo.cls")).unwrap();
        let dead = dead_symbols_in_file(&program, file);
        std::fs::remove_dir_all(&dir).ok();
        (program, file, dead)
    }

    fn dead_names(test_name: &str, src: &str) -> Vec<String> {
        dead_symbols(test_name, src).2.into_iter().map(|d| d.name).collect()
    }

    fn apply_deletion(text: &str, range: TextRange) -> String {
        let start = usize::from(range.start());
        let end = usize::from(range.end());
        format!("{}{}", &text[..start], &text[end..])
    }

    #[test]
    fn unused_private_method_is_flagged() {
        let src = "public class Foo {\n    private void helper() { }\n}\n";
        assert_eq!(dead_names("unused-private-method", src), vec!["helper"]);
    }

    #[test]
    fn used_private_method_is_not_flagged() {
        let src = "public class Foo {\n    private void helper() { }\n    public void run() { helper(); }\n}\n";
        assert!(dead_names("used-private-method", src).is_empty());
    }

    #[test]
    fn public_method_with_zero_callers_is_not_flagged() {
        // Out of scope for v1 -- apex-discover can't see Visualforce/LWC/
        // Flow, so a public member could always be called from somewhere
        // this binder never parses.
        let src = "public class Foo {\n    public void helper() { }\n}\n";
        assert!(dead_names("public-zero-callers", src).is_empty());
    }

    #[test]
    fn private_zero_arg_constructor_is_not_flagged() {
        // The standard "block external instantiation" idiom -- deleting
        // it would silently make the class instantiable again.
        let src = "public class Foo {\n    private Foo() { }\n}\n";
        assert!(dead_names("private-ctor", src).is_empty());
    }

    #[test]
    fn is_test_annotated_private_method_is_not_flagged() {
        let src = "public class Foo {\n    @isTest\n    private static void testSomething() { }\n}\n";
        assert!(dead_names("isTest-annotation", src).is_empty());
    }

    #[test]
    fn legacy_testmethod_modifier_is_not_flagged() {
        let src = "public class Foo {\n    private static testMethod void testSomething() { }\n}\n";
        assert!(dead_names("legacy-testmethod", src).is_empty());
    }

    #[test]
    fn unused_foreach_variable_is_not_flagged() {
        let src = "public class Foo {\n    public void run(List<Integer> xs) {\n        for (Integer x : xs) { }\n    }\n}\n";
        assert!(dead_names("unused-foreach-var", src).is_empty());
    }

    #[test]
    fn unused_private_field_with_doc_comment_deletes_the_whole_declaration() {
        let src = "public class Foo {\n    /** unused */\n    private Integer x;\n    public void run() { }\n}\n";
        let (_, _, dead) = dead_symbols("field-with-doc-comment", src);
        assert_eq!(dead.len(), 1);
        let after = apply_deletion(src, dead[0].deletion_range);
        assert_eq!(after, "public class Foo {\n    public void run() { }\n}\n");
    }

    #[test]
    fn unused_middle_declarator_among_siblings_deletes_only_that_one() {
        let src = "public class Foo {\n    private Integer x, y, z;\n    public void run() { System.debug(x); System.debug(z); }\n}\n";
        let (_, _, dead) = dead_symbols("middle-declarator", src);
        assert_eq!(dead.iter().map(|d| d.name.as_str()).collect::<Vec<_>>(), vec!["y"]);
        let after = apply_deletion(src, dead[0].deletion_range);
        assert_eq!(
            after,
            "public class Foo {\n    private Integer x, z;\n    public void run() { System.debug(x); System.debug(z); }\n}\n"
        );
    }

    #[test]
    fn unused_last_declarator_deletes_the_preceding_comma() {
        let src = "public class Foo {\n    private Integer x, y;\n    public void run() { System.debug(x); }\n}\n";
        let (_, _, dead) = dead_symbols("last-declarator", src);
        assert_eq!(dead.iter().map(|d| d.name.as_str()).collect::<Vec<_>>(), vec!["y"]);
        let after = apply_deletion(src, dead[0].deletion_range);
        assert_eq!(
            after,
            "public class Foo {\n    private Integer x;\n    public void run() { System.debug(x); }\n}\n"
        );
    }

    #[test]
    fn unused_local_variable_is_flagged_and_deletes_cleanly() {
        let src = "public class Foo {\n    public void run() {\n        Integer unused = 5;\n        System.debug('hi');\n    }\n}\n";
        let (_, _, dead) = dead_symbols("unused-local", src);
        assert_eq!(dead.iter().map(|d| d.name.as_str()).collect::<Vec<_>>(), vec!["unused"]);
        let after = apply_deletion(src, dead[0].deletion_range);
        assert_eq!(
            after,
            "public class Foo {\n    public void run() {\n        System.debug('hi');\n    }\n}\n"
        );
    }

    /// The boundary case `compute_deletion_range`'s own doc comment flags
    /// as needing an empirical check: deleting the *last* statement in a
    /// method body, immediately before the closing `}`, must not leave a
    /// blank line behind.
    #[test]
    fn unused_local_variable_as_the_last_statement_leaves_no_blank_line() {
        let src = "public class Foo {\n    public void run() {\n        System.debug('hi');\n        Integer unused = 5;\n    }\n}\n";
        let (_, _, dead) = dead_symbols("unused-local-last-stmt", src);
        assert_eq!(dead.iter().map(|d| d.name.as_str()).collect::<Vec<_>>(), vec!["unused"]);
        let after = apply_deletion(src, dead[0].deletion_range);
        assert_eq!(
            after,
            "public class Foo {\n    public void run() {\n        System.debug('hi');\n    }\n}\n"
        );
    }

    #[test]
    fn dead_code_actions_returns_a_quickfix_that_applies_cleanly() {
        let src = "public class Foo {\n    private void helper() { }\n}\n";
        let dir = write_fixture("code-action-quickfix", src);
        let program = BoundProgram::from_files(&dir);
        let file = program.file_id(&dir.join("Foo.cls")).unwrap();
        let text = program.syntax(file).text().to_string();
        let index = LineIndex::new(&text);
        // A range covering `helper`'s own name (line 1, "private void
        // helper() { }") -- anywhere overlapping the declaration's name
        // should surface its quick-fix.
        let point = Range {
            start: index.to_position(&text, 0, PositionEncoding::Utf16),
            end: index.to_position(&text, text.len() as u32, PositionEncoding::Utf16),
        };
        let actions = dead_code_actions(&program, file, point, PositionEncoding::Utf16);
        std::fs::remove_dir_all(&dir).ok();
        assert_eq!(actions.len(), 1);
        let CodeActionOrCommand::CodeAction(action) = &actions[0] else {
            panic!("expected a CodeAction, not a Command");
        };
        assert!(action.title.contains("helper"));
        let edits = action
            .edit
            .as_ref()
            .and_then(|e| e.changes.as_ref())
            .and_then(|c| c.values().next())
            .expect("expected exactly one file's worth of edits");
        assert_eq!(edits.len(), 1);
        assert_eq!(edits[0].new_text, "");
    }

    /// Real-corpus smoke test: `dead_symbols_in_file` must run to
    /// completion, without panicking, across every file in the real NPSP
    /// checkout (the scale that's actually exposed real bugs in this
    /// codebase before -- see `crates/apexls-server/tests/rename_then_body_edits.rs`),
    /// and it must stay conservative in aggregate -- a wildly overzealous
    /// detector flagging a large share of private members would be a
    /// real regression worth catching here rather than discovering it
    /// live against a real project.
    #[test]
    fn npsp_corpus_dead_symbol_sweep_stays_conservative() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("tests")
            .join("corpus")
            .join("npsp");
        let Ok(root) = root.canonicalize() else {
            eprintln!("skipping: real NPSP corpus checkout not present at {root:?}");
            return;
        };
        let program = BoundProgram::from_files(&root);
        let candidate_count = program
            .symbols
            .iter()
            .filter(|(_, s)| is_dead_code_candidate_kind(s))
            .count();
        let files: std::collections::HashSet<FileId> =
            program.symbols.iter().map(|(_, s)| s.file).collect();
        let dead_count: usize = files.iter().map(|&file| dead_symbols_in_file(&program, file).len()).sum();
        assert!(
            candidate_count > 0,
            "expected at least some private methods/fields/properties/locals in a real corpus this size"
        );
        let ratio = dead_count as f64 / candidate_count as f64;
        assert!(
            ratio < 0.5,
            "flagged {dead_count}/{candidate_count} ({:.0}%) of eligible private members/locals as \
             dead -- suspiciously high, likely an overzealous detector rather than a real finding",
            ratio * 100.0
        );
    }
}
