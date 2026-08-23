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
//! oversight). Deliberately **not** yet wired, each a separate tracked
//! `BACKLOG.md` item rather than silently dropped:
//! - workspace folders beyond a single root (no multi-root support),
//! - `workspace/didChangeConfiguration`,
//! - position-encoding negotiation (`positionEncodingKind`),
//! - request cancellation,
//! - any real language feature at all (hover/goto-definition/etc, or
//!   anything touching `apex-binder`) -- this pass only proves the
//!   protocol loop itself works.

use std::collections::HashMap;
use std::ops::ControlFlow;

use async_lsp::client_monitor::ClientProcessMonitorLayer;
use async_lsp::concurrency::ConcurrencyLayer;
use async_lsp::panic::CatchUnwindLayer;
use async_lsp::router::Router;
use async_lsp::server::LifecycleLayer;
use async_lsp::tracing::TracingLayer;
use async_lsp::{ClientSocket, LanguageServer, ResponseError};
use futures::future::BoxFuture;
use lsp_types::{
    DidChangeTextDocumentParams, DidCloseTextDocumentParams, DidOpenTextDocumentParams,
    DidSaveTextDocumentParams, InitializeParams, InitializeResult, InitializedParams,
    ServerCapabilities, ServerInfo, TextDocumentSyncCapability, TextDocumentSyncKind, Url,
};
use tower::ServiceBuilder;
use tracing::{info, warn, Level};

/// The server's whole mutable state: for now, just an in-memory
/// document store (URI -> current full text, kept in sync via
/// full-document `didChange` notifications). Will grow to hold an
/// `apex_binder::BoundProgram` once a real language feature is wired
/// in -- deliberately not yet, to keep this first pass scoped to the
/// protocol layer alone (see the module doc comment).
struct Backend {
    #[allow(dead_code)] // not sent anything yet -- kept for the features this scaffolds toward
    client: ClientSocket,
    documents: HashMap<Url, String>,
}

impl LanguageServer for Backend {
    type Error = ResponseError;
    type NotifyResult = ControlFlow<async_lsp::Result<()>>;

    fn initialize(
        &mut self,
        params: InitializeParams,
    ) -> BoxFuture<'static, Result<InitializeResult, Self::Error>> {
        info!(
            workspace_folder_count = params.workspace_folders.as_ref().map(Vec::len),
            "initialize",
        );
        // Only the first workspace folder / root_uri is ever honored --
        // multi-root support is a separate, tracked `BACKLOG.md` item.
        if params
            .workspace_folders
            .as_deref()
            .is_some_and(|f| f.len() > 1)
        {
            warn!(
                "multiple workspace folders were offered; only a single root is supported \
                 (multi-root support is not implemented yet)"
            );
        }

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
                    ..ServerCapabilities::default()
                },
                server_info: Some(ServerInfo {
                    name: "apexls".into(),
                    version: Some(env!("CARGO_PKG_VERSION").into()),
                }),
            })
        })
    }

    fn initialized(&mut self, _: InitializedParams) -> Self::NotifyResult {
        info!("initialized");
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
        ControlFlow::Continue(())
    }

    fn did_close(&mut self, params: DidCloseTextDocumentParams) -> Self::NotifyResult {
        let uri = params.text_document.uri;
        info!(%uri, "did_close");
        self.documents.remove(&uri);
        ControlFlow::Continue(())
    }

    fn did_save(&mut self, params: DidSaveTextDocumentParams) -> Self::NotifyResult {
        info!(uri = %params.text_document.uri, "did_save");
        ControlFlow::Continue(())
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
            documents: HashMap::new(),
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
