//! Whole-corpus declaration-site invariant test, modeled on
//! `apex-parser/tests/ast_smoke.rs`'s pattern: bind every real NPSP
//! file, assert zero panics, then assert floor counts per `SymbolKind`
//! (not exact counts -- the point is "an accessor family didn't
//! silently break," not pinning the corpus's exact shape) plus two
//! structural invariants that would catch a real collection/remapping
//! bug: every symbol's `container` (when present) points at a symbol of
//! a kind that can actually contain it, and every symbol's `ptr`
//! resolves back to a real node in its own file's freshly-fetched
//! syntax tree (proving `SyntaxPtr`/`AstPtr`'s re-resolution path works
//! end-to-end on real trees, not just hand-built fixtures).

use apex_binder::{BoundProgram, SymbolKind};
use std::path::{Path, PathBuf};

fn corpus_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/corpus/npsp")
}

#[derive(Default, Debug)]
struct Counts {
    class: usize,
    interface: usize,
    enum_: usize,
    enum_constant: usize,
    trigger: usize,
    method: usize,
    constructor: usize,
    field: usize,
    property: usize,
    parameter: usize,
}

#[test]
fn every_real_npsp_declaration_collects_and_every_pointer_resolves() {
    let root = corpus_root();
    assert!(
        root.exists(),
        "no NPSP corpus found at {root:?}; is the submodule checked out? (git submodule update --init --recursive)"
    );

    let program = BoundProgram::from_files(&root);
    assert!(
        program.file_count() > 1000,
        "expected the whole NPSP corpus to be discovered, got {} files",
        program.file_count()
    );

    let mut counts = Counts::default();
    for (_, symbol) in program.symbols.iter() {
        match symbol.kind {
            SymbolKind::Class => counts.class += 1,
            SymbolKind::Interface => counts.interface += 1,
            SymbolKind::Enum => counts.enum_ += 1,
            SymbolKind::EnumConstant => counts.enum_constant += 1,
            SymbolKind::Trigger => counts.trigger += 1,
            SymbolKind::Method => counts.method += 1,
            SymbolKind::Constructor => counts.constructor += 1,
            SymbolKind::Field => counts.field += 1,
            SymbolKind::Property => counts.property += 1,
            SymbolKind::Parameter => counts.parameter += 1,
            // Locals/catch-vars/for-each-vars/switch-bindings are added
            // in Pass 2, not Pass 1 -- out of scope for this
            // declaration-site test, covered by `scope_resolution_smoke`.
            SymbolKind::LocalVar
            | SymbolKind::CatchVar
            | SymbolKind::ForEachVar
            | SymbolKind::SwitchBindingVar => {}
        }

        // Structural invariant: a symbol's `container`, when present,
        // must point at a symbol whose kind can actually contain it --
        // catches a Pass-1-local-index-remap bug that would otherwise
        // silently wire a symbol to the wrong (or a nonsensical)
        // container.
        if let Some(container) = symbol.container {
            let container_kind = program.symbols.get(container).kind;
            let container_is_valid = match symbol.kind {
                SymbolKind::Method
                | SymbolKind::Constructor
                | SymbolKind::Field
                | SymbolKind::Property
                | SymbolKind::EnumConstant
                | SymbolKind::Class
                | SymbolKind::Interface
                | SymbolKind::Enum => matches!(
                    container_kind,
                    SymbolKind::Class
                        | SymbolKind::Interface
                        | SymbolKind::Enum
                        | SymbolKind::Trigger
                ),
                // `Parameter` also legitimately sits under a `Property`:
                // a `set` accessor's own implicit `value` parameter is
                // collected under the property's id specifically so it
                // stays invisible to ordinary member lookup on the
                // enclosing class (`crate::collect::collect_property`).
                SymbolKind::Parameter => matches!(
                    container_kind,
                    SymbolKind::Method | SymbolKind::Constructor | SymbolKind::Property
                ),
                SymbolKind::LocalVar
                | SymbolKind::CatchVar
                | SymbolKind::ForEachVar
                | SymbolKind::SwitchBindingVar => {
                    matches!(container_kind, SymbolKind::Method | SymbolKind::Constructor)
                }
                SymbolKind::Trigger => false, // triggers never have a container
            };
            assert!(
                container_is_valid,
                "symbol {:?} ({:?}) has container of invalid kind {:?}",
                symbol.name, symbol.kind, container_kind
            );
        }

        // Structural invariant: every declaration's own pointer must
        // resolve back to a real node in its file's syntax tree.
        let root_node = program.syntax(symbol.file);
        assert!(
            symbol.ptr.to_node(&root_node).is_some(),
            "symbol {:?} ({:?})'s ptr did not resolve against its own file's syntax tree",
            symbol.name,
            symbol.kind
        );
    }

    assert!(
        counts.class > 800,
        "expected >800 classes, got {}",
        counts.class
    );
    assert!(
        counts.interface > 30,
        "expected >30 interfaces, got {}",
        counts.interface
    );
    assert!(
        counts.enum_ > 30,
        "expected >30 enums, got {}",
        counts.enum_
    );
    assert!(
        counts.enum_constant > 200,
        "expected >200 enum constants, got {}",
        counts.enum_constant
    );
    assert!(
        counts.trigger > 10,
        "expected >10 triggers, got {}",
        counts.trigger
    );
    assert!(
        counts.method > 8000,
        "expected >8000 methods, got {}",
        counts.method
    );
    assert!(
        counts.constructor > 400,
        "expected >400 constructors, got {}",
        counts.constructor
    );
    assert!(
        counts.field > 2000,
        "expected >2000 fields, got {}",
        counts.field
    );
    assert!(
        counts.property > 500,
        "expected >500 properties, got {}",
        counts.property
    );
    assert!(
        counts.parameter > 5000,
        "expected >5000 parameters, got {}",
        counts.parameter
    );
}
