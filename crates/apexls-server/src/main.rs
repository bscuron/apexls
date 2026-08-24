//! apexls's LSP server binary: the protocol layer described in
//! `BACKLOG.md` §1. Built on `async-lsp` (chosen over `tower-lsp`/
//! `tower-lsp-server` because it handles notifications *synchronously*,
//! matching what the LSP spec actually requires -- both tower-lsp
//! variants process notifications asynchronously, which can reorder
//! e.g. two `didChange` notifications relative to each other).
//!
//! Scope of this first pass, matching `BACKLOG.md` §1's own checklist:
//! the `initialize`/`initialized`/`shutdown`/`exit` handshake, and
//! `textDocument/didOpen`/`didChange`/`didClose`/`didSave` document
//! sync (full-document sync, not incremental -- see `Backend::initialize`'s
//! doc comment for why that's a deliberate, tracked choice, not an
//! oversight).
//!
//! **Single-root only, by design, not by omission.** LSP lets a client
//! offer several `workspaceFolders` at once (VS Code's "multi-root
//! workspace" feature -- e.g. two unrelated repos opened together in
//! one window). apexls doesn't support that: an SFDX org is one flat
//! Apex namespace (`apex_binder::SymbolTable::top_level` is
//! project-wide, not per-file, on purpose), so merging two *unrelated*
//! projects' symbols into one `BoundProgram` would be actively wrong
//! (colliding names, references falsely resolving across projects that
//! have nothing to do with each other) -- and LSP gives no signal to
//! tell "these folders are the same org" apart from "these just happen
//! to be open together." Rather than guess, `Backend::initialize` takes
//! only the first workspace folder and ignores the rest.
//!
//! Request **cancellation** (`$/cancelRequest`) needs no code here at
//! all: `ConcurrencyLayer`, wired into the middleware stack below,
//! already intercepts that notification and aborts the matching
//! in-flight request's future automatically (confirmed by reading
//! `async-lsp`'s own source -- see `BACKLOG.md` §1). There's no
//! end-to-end test of it *actually cancelling something* yet, since
//! every request handler so far completes near-instantly; that'll
//! become naturally testable once a real (potentially slow,
//! binder-backed) request exists.
//!
//! **`apex-binder` integration (`BACKLOG.md` §2 Step 1).** `Backend`
//! keeps a background-rebuilt `apex_binder::BoundProgram` (`Backend::bind`,
//! `Backend::schedule_rebuild`) in sync with `self.root`/`self.documents`,
//! naively -- every `didOpen`/`didChange`/`didClose` triggers a full
//! project rebuild on a `spawn_blocking` task, so the main loop never
//! blocks on it, but there's no debouncing or cancellation of a
//! rebuild a newer edit has already superseded yet (see
//! `Backend::schedule_rebuild`'s doc comment). Unaffected files' parses
//! and binder output are reused across rebuilds via `apex_binder::BindCache`
//! (`BACKLOG.md` §2's incremental rebind). Deliberately **not**
//! yet wired, each a separate tracked `BACKLOG.md` §3 item rather than
//! silently dropped: any real language feature that would *consume* the
//! bind (hover/goto-definition/etc) -- this pass only proves a bind
//! happens and can be measured.

use std::collections::HashMap;
use std::ops::ControlFlow;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock};

use apex_binder::{BindCache, BoundProgram, Resolution};
use async_lsp::client_monitor::ClientProcessMonitorLayer;
use async_lsp::concurrency::ConcurrencyLayer;
use async_lsp::panic::CatchUnwindLayer;
use async_lsp::router::Router;
use async_lsp::server::LifecycleLayer;
use async_lsp::tracing::TracingLayer;
use async_lsp::{ClientSocket, LanguageServer, ResponseError};
use futures::future::BoxFuture;
use lsp_types::{
    DidChangeConfigurationParams, DidChangeTextDocumentParams, DidCloseTextDocumentParams,
    DidOpenTextDocumentParams, DidSaveTextDocumentParams, DocumentSymbolParams,
    DocumentSymbolResponse, FoldingRange, FoldingRangeParams, FoldingRangeProviderCapability,
    GotoDefinitionParams, GotoDefinitionResponse, Hover, HoverContents, HoverParams,
    HoverProviderCapability, InitializeParams, InitializeResult, InitializedParams,
    MarkupContent, MarkupKind, OneOf, SelectionRange, SelectionRangeParams,
    SelectionRangeProviderCapability, ServerCapabilities, ServerInfo, TextDocumentSyncCapability,
    TextDocumentSyncKind, Url, WorkspaceSymbolParams, WorkspaceSymbolResponse,
};
use tower::ServiceBuilder;
use tracing::{info, warn, Level};

mod capabilities;
mod line_index;

use line_index::PositionEncoding;

/// The server's whole mutable state: the single resolved project root
/// (see the module doc comment's "single-root only" section) and an
/// in-memory document store (URI -> current full text, kept in sync via
/// full-document `didChange` notifications). Will grow to hold an
/// `apex_binder::BoundProgram` once a real language feature is wired
/// in -- deliberately not yet, to keep this first pass scoped to the
/// protocol layer alone (see the module doc comment).
struct Backend {
    #[allow(dead_code)] // not sent anything yet -- kept for the features this scaffolds toward
    client: ClientSocket,
    /// Resolved once in `initialize` from the first `workspaceFolders`
    /// entry (falling back to the deprecated `rootUri` for older
    /// clients that don't send `workspaceFolders` at all). `None` if
    /// the client offered neither -- a request without any open folder,
    /// which every LSP client allows for single-file editing.
    root: Option<Url>,
    /// Negotiated once in `initialize` (see `line_index::PositionEncoding::negotiate`).
    /// `Utf16` until then, matching the LSP-mandated default -- never
    /// actually observed pre-negotiation, since nothing sends a
    /// position-bearing request before `initialize` completes.
    position_encoding: PositionEncoding,
    /// The raw `initializationOptions` blob from `initialize`, updated
    /// wholesale by any later `workspace/didChangeConfiguration`
    /// notification. Kept as opaque JSON rather than a typed config
    /// struct since nothing consumes specific settings yet (no
    /// standard-library-stub toggle, no metadata-root override) --
    /// see `BACKLOG.md` §4 for what those settings will eventually be.
    config: Option<serde_json::Value>,
    documents: HashMap<Url, String>,
    /// The current `apex-binder` bind, rebuilt in the background (see
    /// `Backend::schedule_rebuild`) after `initialized` and every
    /// document-sync notification. Every edit schedules a rebuild, but
    /// `bind.cache` (`apex_binder::BindCache`) makes each one
    /// incremental -- unaffected files' parses *and* declarations/
    /// references are reused, not just reparsed -- so in the common case
    /// (an edit that doesn't change any declaration) only the edited
    /// file's own bodies actually get re-resolved. Still no debouncing or
    /// cancellation of a still-running rebuild a newer edit has already
    /// superseded. `None` until the first rebuild completes. Not
    /// consumed by any capability yet (that's `BACKLOG.md` §3) -- this
    /// wiring exists so real single-edit rebuild latency can be measured
    /// instead of guessed.
    bind: Arc<BindState>,
}

/// The mutable state a background rebuild task needs, shared via `Arc`
/// so `Backend::schedule_rebuild` can hand a clone to
/// `tokio::task::spawn_blocking` without borrowing `Backend` itself
/// across an async boundary. `std::sync::Mutex`/`RwLock`, not `tokio`'s --
/// every access happens either on a blocking-pool thread (the rebuild
/// itself) or held only long enough to swap a value (never held across
/// an `.await`), so there's no blocking-executor hazard to avoid.
#[derive(Default)]
struct BindState {
    program: RwLock<Option<BoundProgram>>,
    cache: Mutex<BindCache>,
}

impl Backend {
    /// Kicks off a background rebuild of `self.bind` against the current
    /// `self.root` and `self.documents` (each open buffer's in-memory
    /// text overriding its on-disk content -- see
    /// `BoundProgram::from_files_with_overrides`'s doc comment for why
    /// that matters). A no-op if there's no root yet (single-file mode,
    /// or a request that raced ahead of `initialize`).
    ///
    /// Fire-and-forget by design for this first pass (`BACKLOG.md` §2
    /// Step 1): the returned `JoinHandle` is dropped, not awaited, so
    /// the rebuild keeps running on `spawn_blocking`'s pool even though
    /// nothing here waits on it, and a burst of rapid edits schedules a
    /// burst of overlapping rebuilds with no cancellation between them
    /// -- whichever finishes last wins (`self.bind.program`'s
    /// `RwLock::write` is the only synchronization). Debouncing and
    /// superseded-rebuild cancellation are exactly the kind of
    /// refinement `BACKLOG.md` §2 Step 3's real latency measurement
    /// should justify (or not) rather than building speculatively now.
    fn schedule_rebuild(&self) {
        let Some(root) = self.root.as_ref().and_then(|url| url.to_file_path().ok()) else {
            return;
        };
        let overrides: HashMap<PathBuf, String> = self
            .documents
            .iter()
            .filter_map(|(uri, text)| uri.to_file_path().ok().map(|path| (path, text.clone())))
            .collect();
        let bind = Arc::clone(&self.bind);
        tokio::task::spawn_blocking(move || {
            let program = {
                let mut cache = bind.cache.lock().unwrap();
                BoundProgram::from_files_cached(&root, &overrides, &mut cache)
            };
            let file_count = program.file_count();
            *bind.program.write().unwrap() = Some(program);
            info!(file_count, "rebuild complete");
        });
    }
}

impl LanguageServer for Backend {
    type Error = ResponseError;
    type NotifyResult = ControlFlow<async_lsp::Result<()>>;

    fn initialize(
        &mut self,
        params: InitializeParams,
    ) -> BoxFuture<'static, Result<InitializeResult, Self::Error>> {
        let folders = params.workspace_folders.as_deref().unwrap_or(&[]);
        let root = folders
            .first()
            .map(|folder| folder.uri.clone())
            .or_else(|| {
                #[allow(deprecated)] // the fallback this deprecation exists for
                params.root_uri.clone()
            });

        let client_encodings = params
            .capabilities
            .general
            .as_ref()
            .and_then(|g| g.position_encodings.as_deref());
        let position_encoding = PositionEncoding::negotiate(client_encodings);

        info!(
            ?root,
            workspace_folder_count = folders.len(),
            ?position_encoding,
            has_initialization_options = params.initialization_options.is_some(),
            "initialize",
        );
        if folders.len() > 1 {
            warn!(
                extra_folders = folders.len() - 1,
                "apexls supports a single project root; additional workspace folders are ignored \
                 (see main.rs's module doc comment for why this is by design)"
            );
        }
        self.root = root;
        self.position_encoding = position_encoding;
        self.config = params.initialization_options;

        Box::pin(async move {
            Ok(InitializeResult {
                capabilities: ServerCapabilities {
                    // Full-document sync, not incremental: the simplest
                    // correct baseline for this first pass. Incremental
                    // reparse/rebind is a real perf item
                    // (`BACKLOG.md` §2), worth doing once there's a
                    // binder-backed feature that would actually benefit
                    // from it -- not before.
                    text_document_sync: Some(TextDocumentSyncCapability::Kind(
                        TextDocumentSyncKind::FULL,
                    )),
                    // Always stated explicitly rather than omitted:
                    // omitting `position_encoding` means both sides must
                    // assume UTF-16 per spec, which is exactly what we'd
                    // pick anyway when the client doesn't offer UTF-8 --
                    // but being explicit means a client inspecting the
                    // response never has to know that default by heart.
                    position_encoding: Some(position_encoding.into()),
                    hover_provider: Some(HoverProviderCapability::Simple(true)),
                    definition_provider: Some(OneOf::Left(true)),
                    document_symbol_provider: Some(OneOf::Left(true)),
                    workspace_symbol_provider: Some(OneOf::Left(true)),
                    folding_range_provider: Some(FoldingRangeProviderCapability::Simple(true)),
                    selection_range_provider: Some(SelectionRangeProviderCapability::Simple(true)),
                    ..ServerCapabilities::default()
                },
                server_info: Some(ServerInfo {
                    name: "apexls".into(),
                    version: Some(env!("CARGO_PKG_VERSION").into()),
                }),
            })
        })
    }

    fn did_change_configuration(
        &mut self,
        params: DidChangeConfigurationParams,
    ) -> Self::NotifyResult {
        info!("did_change_configuration");
        self.config = Some(params.settings);
        ControlFlow::Continue(())
    }

    fn initialized(&mut self, _: InitializedParams) -> Self::NotifyResult {
        info!("initialized");
        self.schedule_rebuild();
        ControlFlow::Continue(())
    }

    fn shutdown(&mut self, _: ()) -> BoxFuture<'static, Result<(), Self::Error>> {
        info!("shutdown");
        Box::pin(async move { Ok(()) })
    }

    fn did_open(&mut self, params: DidOpenTextDocumentParams) -> Self::NotifyResult {
        let uri = params.text_document.uri;
        info!(%uri, "did_open");
        self.documents.insert(uri, params.text_document.text);
        self.schedule_rebuild();
        ControlFlow::Continue(())
    }

    fn did_change(&mut self, params: DidChangeTextDocumentParams) -> Self::NotifyResult {
        let uri = params.text_document.uri;
        // Full sync only (see `initialize`): the client always sends
        // exactly one change event whose `text` is the whole new
        // document, so this replaces the stored copy outright rather
        // than applying a range-based patch.
        let Some(change) = params.content_changes.into_iter().next() else {
            return ControlFlow::Continue(());
        };
        info!(%uri, len = change.text.len(), "did_change");
        self.documents.insert(uri, change.text);
        self.schedule_rebuild();
        ControlFlow::Continue(())
    }

    fn did_close(&mut self, params: DidCloseTextDocumentParams) -> Self::NotifyResult {
        let uri = params.text_document.uri;
        info!(%uri, "did_close");
        self.documents.remove(&uri);
        self.schedule_rebuild();
        ControlFlow::Continue(())
    }

    fn did_save(&mut self, params: DidSaveTextDocumentParams) -> Self::NotifyResult {
        info!(uri = %params.text_document.uri, "did_save");
        ControlFlow::Continue(())
    }

    /// `BACKLOG.md` §3's first real consumer of the bind: what's the
    /// user's cursor on. Tries a declaration's own name first
    /// (`BoundProgram::symbol_at`), then falls back to a reference's
    /// resolution (`BoundProgram::resolution_at`) -- see
    /// `capabilities::describe_symbol`'s doc comment for how a `Symbol`
    /// becomes hover text. `Candidates` (no overload narrowing for a bare
    /// name) shows the first candidate plus an honest "+N more" note
    /// rather than silently picking one; `SchemaObject`/`UnknownSchema`/
    /// `Unresolved`/no bind yet all fall through to no hover, matching
    /// this binder's existing honesty about the still-unmodeled stdlib/
    /// schema surface (`BACKLOG.md` §4).
    fn hover(
        &mut self,
        params: HoverParams,
    ) -> BoxFuture<'static, Result<Option<Hover>, Self::Error>> {
        let uri = params.text_document_position_params.text_document.uri;
        let position = params.text_document_position_params.position;
        let encoding = self.position_encoding;
        let bind = Arc::clone(&self.bind);
        Box::pin(async move {
            let program_guard = bind.program.read().unwrap();
            let Some(program) = program_guard.as_ref() else {
                return Ok(None);
            };
            let Some((file, offset)) =
                capabilities::resolve_position(program, &uri, position, encoding)
            else {
                return Ok(None);
            };

            let value = if let Some(id) = program.symbol_at(file, offset) {
                Some(capabilities::describe_symbol(program, id))
            } else {
                match program.resolution_at(file, offset) {
                    Some(Resolution::Resolved(id)) => {
                        Some(capabilities::describe_symbol(program, *id))
                    }
                    Some(Resolution::Candidates(ids)) => ids.first().map(|&id| {
                        let mut text = capabilities::describe_symbol(program, id);
                        if ids.len() > 1 {
                            text.push_str(&format!("\n\n*+{} more overload(s)*", ids.len() - 1));
                        }
                        text
                    }),
                    _ => None,
                }
            };

            Ok(value.map(|value| Hover {
                contents: HoverContents::Markup(MarkupContent {
                    kind: MarkupKind::Markdown,
                    value,
                }),
                range: None,
            }))
        })
    }

    /// Only ever follows a *reference*'s resolution
    /// (`BoundProgram::resolution_at`), not a declaration's own name
    /// (`symbol_at`) -- "go to definition" on your own declaration has
    /// nowhere useful to go, so that case stays `None` rather than being
    /// specially handled, matching common LSP server behavior.
    fn definition(
        &mut self,
        params: GotoDefinitionParams,
    ) -> BoxFuture<'static, Result<Option<GotoDefinitionResponse>, Self::Error>> {
        let uri = params.text_document_position_params.text_document.uri;
        let position = params.text_document_position_params.position;
        let encoding = self.position_encoding;
        let bind = Arc::clone(&self.bind);
        Box::pin(async move {
            let program_guard = bind.program.read().unwrap();
            let Some(program) = program_guard.as_ref() else {
                return Ok(None);
            };
            let Some((file, offset)) =
                capabilities::resolve_position(program, &uri, position, encoding)
            else {
                return Ok(None);
            };

            let response = match program.resolution_at(file, offset) {
                Some(Resolution::Resolved(id)) => {
                    capabilities::symbol_location(program, *id, encoding)
                        .map(GotoDefinitionResponse::Scalar)
                }
                Some(Resolution::Candidates(ids)) => {
                    let locations: Vec<_> = ids
                        .iter()
                        .filter_map(|&id| capabilities::symbol_location(program, id, encoding))
                        .collect();
                    (!locations.is_empty()).then_some(GotoDefinitionResponse::Array(locations))
                }
                Some(Resolution::SchemaObject(r)) => capabilities::schema_location(program, r)
                    .map(GotoDefinitionResponse::Scalar),
                _ => None,
            };

            Ok(response)
        })
    }

    /// The outline view: `capabilities::document_symbols` nests every
    /// declaration-shaped symbol in `file` by `Symbol::container`.
    fn document_symbol(
        &mut self,
        params: DocumentSymbolParams,
    ) -> BoxFuture<'static, Result<Option<DocumentSymbolResponse>, Self::Error>> {
        let uri = params.text_document.uri;
        let encoding = self.position_encoding;
        let bind = Arc::clone(&self.bind);
        Box::pin(async move {
            let program_guard = bind.program.read().unwrap();
            let Some(program) = program_guard.as_ref() else {
                return Ok(None);
            };
            let Some(path) = uri.to_file_path().ok() else {
                return Ok(None);
            };
            let Some(file) = program.file_id(&path) else {
                return Ok(None);
            };
            let symbols = capabilities::document_symbols(program, file, encoding);
            Ok((!symbols.is_empty()).then_some(DocumentSymbolResponse::Nested(symbols)))
        })
    }

    /// Project-wide symbol search: `capabilities::workspace_symbols`'s
    /// case-insensitive substring match over every declaration-shaped
    /// symbol in the current bind.
    fn symbol(
        &mut self,
        params: WorkspaceSymbolParams,
    ) -> BoxFuture<'static, Result<Option<WorkspaceSymbolResponse>, Self::Error>> {
        let encoding = self.position_encoding;
        let bind = Arc::clone(&self.bind);
        Box::pin(async move {
            let program_guard = bind.program.read().unwrap();
            let Some(program) = program_guard.as_ref() else {
                return Ok(None);
            };
            let results = capabilities::workspace_symbols(program, &params.query, encoding);
            Ok((!results.is_empty()).then_some(WorkspaceSymbolResponse::Flat(results)))
        })
    }

    /// Every brace-delimited region in `file` -- `capabilities::folding_ranges`
    /// works straight off the CST, no bind needed, so this only ever
    /// comes back empty for a genuinely unknown file, not a stale one.
    fn folding_range(
        &mut self,
        params: FoldingRangeParams,
    ) -> BoxFuture<'static, Result<Option<Vec<FoldingRange>>, Self::Error>> {
        let uri = params.text_document.uri;
        let bind = Arc::clone(&self.bind);
        Box::pin(async move {
            let program_guard = bind.program.read().unwrap();
            let Some(program) = program_guard.as_ref() else {
                return Ok(None);
            };
            let Some(path) = uri.to_file_path().ok() else {
                return Ok(None);
            };
            let Some(file) = program.file_id(&path) else {
                return Ok(None);
            };
            let ranges = capabilities::folding_ranges(program, file);
            Ok((!ranges.is_empty()).then_some(ranges))
        })
    }

    /// One expanding-selection chain per requested position
    /// (`capabilities::selection_range_at`) -- a position `resolve_position`
    /// can't place (outside the known file/text) still gets a trivial
    /// zero-width range back rather than being dropped, since the
    /// response array must stay the same length as `params.positions`.
    fn selection_range(
        &mut self,
        params: SelectionRangeParams,
    ) -> BoxFuture<'static, Result<Option<Vec<SelectionRange>>, Self::Error>> {
        let uri = params.text_document.uri;
        let positions = params.positions;
        let encoding = self.position_encoding;
        let bind = Arc::clone(&self.bind);
        Box::pin(async move {
            let program_guard = bind.program.read().unwrap();
            let Some(program) = program_guard.as_ref() else {
                return Ok(None);
            };
            let ranges: Vec<SelectionRange> = positions
                .into_iter()
                .map(|position| {
                    capabilities::resolve_position(program, &uri, position, encoding)
                        .and_then(|(file, offset)| {
                            capabilities::selection_range_at(program, file, offset, encoding)
                        })
                        .unwrap_or(SelectionRange {
                            range: lsp_types::Range {
                                start: position,
                                end: position,
                            },
                            parent: None,
                        })
                })
                .collect();
            Ok(Some(ranges))
        })
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    tracing_subscriber::fmt()
        .with_max_level(Level::INFO)
        .with_ansi(false)
        // LSP servers talk to the client over stdout -- all logging
        // must go to stderr, never stdout, or it corrupts the
        // Content-Length-framed JSON-RPC stream.
        .with_writer(std::io::stderr)
        .init();

    let (server, _) = async_lsp::MainLoop::new_server(|client| {
        let router = Router::from_language_server(Backend {
            client: client.clone(),
            root: None,
            position_encoding: PositionEncoding::Utf16,
            config: None,
            documents: HashMap::new(),
            bind: Arc::new(BindState::default()),
        });

        ServiceBuilder::new()
            .layer(TracingLayer::default())
            .layer(LifecycleLayer::default())
            .layer(CatchUnwindLayer::default())
            .layer(ConcurrencyLayer::default())
            .layer(ClientProcessMonitorLayer::new(client))
            .service(router)
    });

    // Prefer truly asynchronous piped stdin/stdout without blocking
    // tasks where the platform supports it.
    #[cfg(unix)]
    let (stdin, stdout) = (
        async_lsp::stdio::PipeStdin::lock_tokio().unwrap(),
        async_lsp::stdio::PipeStdout::lock_tokio().unwrap(),
    );
    // Fall back to spawn-blocking read/write elsewhere (Windows).
    #[cfg(not(unix))]
    let (stdin, stdout) = (
        tokio_util::compat::TokioAsyncReadCompatExt::compat(tokio::io::stdin()),
        tokio_util::compat::TokioAsyncWriteCompatExt::compat_write(tokio::io::stdout()),
    );

    server.run_buffered(stdin, stdout).await.unwrap();
}
