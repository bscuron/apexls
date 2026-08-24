//! Shared plumbing for position-based capabilities (`textDocument/hover`,
//! `textDocument/definition`, and whatever else `BACKLOG.md` §3 adds
//! next): resolving an LSP `(Url, Position)` down to the coordinates
//! `apex_binder::BoundProgram`'s own API understands, and the reverse --
//! turning a resolved `SymbolId` back into an LSP `Location`.

use crate::line_index::{LineIndex, PositionEncoding};
use apex_binder::{BoundProgram, FileId, SchemaObjectRef, SymbolId, SymbolKind, Visibility};
use apex_syntax::ast::decl::{
    ClassDecl, ConstructorDecl, EnumDecl, FieldDecl, HasDocComment, InterfaceDecl, MethodDecl,
    PropertyDecl, TriggerUnit,
};
use lsp_types::{Location, Position, Range, Url};
use rowan::TextSize;

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
