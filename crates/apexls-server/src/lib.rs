//! apexls's LSP server: the protocol layer described in `BACKLOG.md`
//! §1, exposed as `run_server()` for the `apexls-server` compatibility
//! binary (`src/main.rs`, unchanged behavior, kept so existing editor
//! configs invoking that binary name directly need no changes) and for
//! the `apexls server` subcommand (`crates/apexls`) alike -- genuinely
//! one implementation, two entry points. Built on `async-lsp` (chosen
//! over `tower-lsp`/
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
//! `Backend::schedule_rebuild`) in sync with `self.root`/`self.bind.documents`,
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
//!
//! **Filesystem watching** (`Backend::start_watcher`). `apexls` owns its
//! own `notify`-based watch on `self.root`, matching rust-analyzer's
//! `vfs-notify` rather than depending on the LSP client to register and
//! reliably deliver `workspace/didChangeWatchedFiles` -- client support
//! for that is inconsistent across editors. Closes the "a file added/
//! removed on disk but never opened in the editor isn't picked up" gap
//! `apex_binder::BoundProgram::from_files_cached`'s own doc comment
//! otherwise accepts as an honest limit. Registration is `spawn_blocking`ed
//! from `initialized` rather than run synchronously in `initialize` --
//! measured at ~330ms against the real NPSP corpus, which used to sit
//! directly in `initialize`'s own response path.

use std::collections::HashMap;
use std::ops::ControlFlow;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use parking_lot::{Mutex, RwLock};

use apex_binder::{BindCache, BoundProgram, Resolution};
use async_lsp::client_monitor::ClientProcessMonitorLayer;
use async_lsp::concurrency::ConcurrencyLayer;
use async_lsp::panic::CatchUnwindLayer;
use async_lsp::router::Router;
use async_lsp::server::LifecycleLayer;
use async_lsp::tracing::TracingLayer;
use async_lsp::{ClientSocket, ErrorCode, LanguageServer, ResponseError};
use futures::future::BoxFuture;
use lsp_types::{
    CallHierarchyIncomingCall, CallHierarchyIncomingCallsParams, CallHierarchyItem,
    CallHierarchyOptions, CallHierarchyOutgoingCall, CallHierarchyOutgoingCallsParams,
    CallHierarchyPrepareParams, CallHierarchyServerCapability, CodeActionKind, CodeActionOptions, CodeActionParams, CodeActionProviderCapability,
    CodeActionResponse, CodeLens, CodeLensOptions, CodeLensParams, CompletionOptions, CompletionParams, CompletionResponse,
    DiagnosticOptions, DiagnosticServerCapabilities, DidChangeConfigurationParams, DidChangeTextDocumentParams,
    DidCloseTextDocumentParams, DidOpenTextDocumentParams, DidSaveTextDocumentParams,
    DocumentDiagnosticParams, DocumentDiagnosticReport, DocumentDiagnosticReportResult,
    DocumentHighlight, DocumentHighlightParams, DocumentSymbolParams, DocumentSymbolResponse,
    ExecuteCommandOptions, ExecuteCommandParams,
    FoldingRange, FoldingRangeParams, FoldingRangeProviderCapability, FullDocumentDiagnosticReport, GotoDefinitionParams,
    GotoDefinitionResponse, Hover, HoverContents, HoverParams, HoverProviderCapability, InlayHint,
    InlayHintParams, InitializeParams, InitializeResult, InitializedParams, Location,
    MarkupContent, MarkupKind, MessageType, NumberOrString, OneOf, PrepareRenameResponse, ProgressParams,
    ProgressParamsValue, PublishDiagnosticsParams,
    ReferenceParams, RelatedFullDocumentDiagnosticReport, RenameOptions,
    RenameParams, SelectionRange, SelectionRangeParams, SelectionRangeProviderCapability,
    SemanticTokensFullOptions, SemanticTokensLegend, SemanticTokensOptions, SemanticTokensParams,
    SemanticTokensRangeParams, SemanticTokensRangeResult, SemanticTokensResult,
    SemanticTokensServerCapabilities,
    ServerCapabilities, ServerInfo, ShowMessageParams, SignatureHelp, SignatureHelpOptions, SignatureHelpParams,
    TextDocumentPositionParams, TextDocumentSyncCapability, TextDocumentSyncKind, Url,
    WorkDoneProgress, WorkDoneProgressBegin, WorkDoneProgressCreateParams, WorkDoneProgressEnd,
    WorkspaceEdit, WorkspaceSymbolParams, WorkspaceSymbolResponse,
};
use rowan::{TextRange, TextSize};
use notify_debouncer_full::notify::{RecommendedWatcher, RecursiveMode};
use notify_debouncer_full::{new_debouncer, DebounceEventResult, Debouncer, RecommendedCache};
use tower::ServiceBuilder;
use tracing::{info, warn, Level};

mod capabilities;
mod fix;
mod line_index;

use line_index::{LineIndex, PositionEncoding};

/// The server's whole mutable state: the single resolved project root
/// (see the module doc comment's "single-root only" section) plus
/// whatever's local to the protocol layer itself. The in-memory document
/// store (URI -> current full text, kept in sync via full-document
/// `didChange` notifications) and the background-rebuilt bind both live
/// on `bind` (`Arc<BindState>`) instead of directly here -- both need to
/// be reachable from the filesystem watcher's own callback thread, which
/// has no access to `&Backend` (see `BindState`'s doc comment).
struct Backend {
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
    /// Negotiated once in `initialize` from `ClientCapabilities.window.work_done_progress`
    /// (spec 3.15's `window/workDoneProgress`). When `false` (an older or
    /// minimal client), `spawn_rebuild_worker` skips the whole
    /// create/begin/end progress dance rather than sending notifications
    /// a client that never asked for them would just discard.
    work_done_progress_supported: bool,
    /// The current `apex-binder` bind, rebuilt in the background by a
    /// single persistent worker task (`spawn_rebuild_worker`) after
    /// `initialized` and every document-sync notification
    /// (`Backend::schedule_rebuild`). Every edit requests a rebuild, but
    /// `bind.cache` (`apex_binder::BindCache`) makes each one
    /// incremental -- unaffected files' parses *and* declarations/
    /// references are reused, not just reparsed -- so in the common case
    /// (an edit that doesn't change any declaration) only the edited
    /// file's own bodies actually get re-resolved. A burst of rapid edits
    /// coalesces into a single rebuild once the worker is free rather
    /// than running (or racing) one per edit -- see `BindState::rebuild_requested`'s
    /// doc comment. `None` until the first rebuild completes. Not
    /// consumed by any capability yet (that's `BACKLOG.md` §3) -- this
    /// wiring exists so real single-edit rebuild latency can be measured
    /// instead of guessed.
    bind: Arc<BindState>,
}

/// The mutable state a background rebuild task needs, shared via `Arc`
/// so `Backend::schedule_rebuild`/the filesystem watcher's callback can
/// each hand a clone to `tokio::task::spawn_blocking` without borrowing
/// `Backend` itself across an async boundary -- the watcher's callback
/// in particular runs on a thread `notify` owns, not one `Backend`'s own
/// `&mut self` methods ever run on, so it needs its own independent
/// handle to this state. `parking_lot::Mutex`/`RwLock`, not `tokio`'s --
/// every access happens either on a blocking-pool thread (the rebuild
/// itself) or held only long enough to swap a value (never held across
/// an `.await`), so there's no blocking-executor hazard to avoid. Not
/// `std::sync`'s either (this crate's original choice): `std`'s locks
/// *poison* -- once a panic unwinds while a guard is held, every later
/// `.lock()`/`.read()`/`.write()` on that same lock also panics,
/// forever, for the rest of the process. `program`'s read guard in
/// particular is held across an entire capability call (`hover`/
/// `completion`/`references`/...); `async_lsp`'s `ConcurrencyLayer`/
/// `CatchUnwindLayer` (`main.rs`) already turn one panicking request
/// into a clean per-request error rather than crashing the process, but
/// poisoning would silently escalate that from "one bad request" to
/// "every request for the rest of the session," with no crash and no
/// signal beyond every later response also erroring. `parking_lot`'s
/// locks never poison -- a panic under a held guard just unwinds
/// normally and the lock is fully usable again on the next request --
/// closing that whole failure class structurally instead of relying on
/// every future capability never panicking under a held guard.
/// `documents` moved here (from `Backend` directly) for the same reason:
/// a rebuild the watcher triggers still needs to know which files are
/// open, unsaved buffers so their in-memory content keeps overriding
/// on-disk content, exactly as a document-sync-triggered rebuild does.
/// `watcher` lives here too, not on `Backend`, for a related but
/// different reason: registering the actual OS-level recursive watch
/// (`Debouncer::watch`) is itself slow on a real project (measured
/// ~330ms against the real NPSP corpus, ~1044 files) and runs on a
/// `spawn_blocking` task kicked off from `initialized` rather than
/// synchronously in `initialize`, so it can't be written back through
/// `&mut self` -- see `Backend::start_watcher`'s doc comment.
struct BindState {
    program: RwLock<Option<BoundProgram>>,
    cache: Mutex<BindCache>,
    /// Every open buffer's in-memory text, plus a `version` bumped by
    /// every `did_open`/`did_change`/`did_close` mutation. Both fields
    /// share one lock deliberately, not two independent ones: a request
    /// handler snapshots `version` (see `Backend::hover` and friends)
    /// while the rebuild worker reads `texts`, and if those lived behind
    /// separate locks the worker could observe a `texts` snapshot that
    /// already reflects an edit while still reading the *previous*
    /// `version` -- under-recording which edit its resulting
    /// `BoundProgram` actually covers. One shared lock makes that pairing
    /// atomic, so `bound_version` (below) is always an honest floor on
    /// what `bind.program` reflects.
    documents: Mutex<Documents>,
    /// The `documents.version` that the currently-published `bind.program`
    /// reflects -- published by `spawn_rebuild_worker` immediately after
    /// swapping `program`, so a `Receiver::wait_for` observing a new
    /// value is guaranteed (by `tokio::sync::watch`'s own synchronization)
    /// to also observe that swap. This is the fix for a real race: a
    /// `documentHighlight`/`hover`/... request dispatched right after a
    /// `didChange` used to be free to read whatever `bind.program`
    /// already happened to contain, with nothing tying the query to the
    /// edit that just preceded it -- so it could (and, observed live
    /// against a real editor, did) answer from the *pre-edit* bind, byte-
    /// for-byte, even though the rebuild that would have fixed it landed
    /// only milliseconds later. Every read-only request now snapshots
    /// `documents.version` before dispatching (synchronously, so it's
    /// guaranteed to already include any preceding `did_change` -- see
    /// this module's own doc comment on notification-before-request
    /// ordering) and awaits `wait_for_rebuild` on it before touching
    /// `program`.
    bound_version: tokio::sync::watch::Sender<u64>,
    /// Whether `spawn_rebuild_worker` was ever actually spawned (a
    /// resolved project root at `initialized` time). `wait_for_rebuild`
    /// checks this first and returns immediately when it's `false`,
    /// since single-file mode never publishes a `bound_version` update at
    /// all -- without this check, any request made after even one edit
    /// would wait forever. `bind.program` staying permanently `None` in
    /// that mode already makes every handler's `let Some(program) = ...
    /// else { return Ok(None) }` the right behavior, unaffected by this
    /// flag.
    worker_active: AtomicBool,
    /// Signals `spawn_rebuild_worker`'s single persistent background
    /// task that `documents` has changed and there's a rebuild to do --
    /// set by `Backend::schedule_rebuild` and the filesystem watcher's
    /// callback, never awaited on by either. `tokio::sync::Notify`
    /// stores at most one buffered "wake up" permit: any number of
    /// `notify_one()` calls that land while the worker is still busy
    /// with a previous rebuild (or hasn't started waiting yet) collapse
    /// into that single permit rather than queuing one wake-up per call.
    /// That's exactly the behavior a burst of rapid edits wants -- once
    /// the worker finishes its current rebuild and loops back to wait
    /// again, it immediately picks up the one outstanding permit and
    /// runs exactly one more rebuild covering everything that changed in
    /// the meantime, rather than replaying every edit in between one at
    /// a time. Unlike a debounce timer, there's no artificial delay
    /// before an *idle* worker picks up a single edit (it starts the
    /// instant `notify_one()` is called) and no tuning constant to pick
    /// -- "coalesce" only ever means "the worker was still busy," never
    /// "wait around in case something happens." See
    /// `spawn_rebuild_worker`'s doc comment for how this also makes the
    /// worker's own `from_files_cached` calls trivially safe to reason
    /// about: with exactly one task ever making them, sequentially,
    /// there is no "two rebuilds raced and one landed out of order"
    /// scenario left to guard against.
    rebuild_requested: tokio::sync::Notify,
    /// A monotonically increasing counter minting a fresh `$/progress`
    /// token for each rebuild (`spawn_rebuild_worker`) -- the LSP spec's
    /// `window/workDoneProgress/create` -> begin -> end lifecycle is
    /// per-token, so reusing one token across rebuilds would leave a
    /// client that already saw that token's `end` unsure whether a later
    /// `begin` on the same token starts a new operation or malformedly
    /// continues the old one.
    progress_seq: AtomicU64,
    /// Kept alive only so the watch stays active -- `Debouncer` stops
    /// watching on drop. `None` until `Backend::start_watcher`'s
    /// `spawn_blocking` task finishes registering it, and permanently
    /// `None` in single-file mode or if the watcher failed to start (a
    /// missing/unreadable root, or the platform's watch API erroring --
    /// logged, not fatal, since `apexls` still works without it via
    /// document-sync-triggered rebuilds alone, just without picking up
    /// out-of-band disk changes). Honest race window: a file added/
    /// removed purely on disk between `initialized` firing and this
    /// finishing registration (real but brief -- ~330ms on the real NPSP
    /// corpus, likely far less on a smaller project) won't be caught
    /// until some *other* trigger (an edit, a later out-of-band change
    /// once the watch is live) causes a rebuild -- strictly better than
    /// before this feature existed, when that window was unbounded.
    watcher: Mutex<Option<Debouncer<RecommendedWatcher, RecommendedCache>>>,
}

impl Default for BindState {
    fn default() -> Self {
        // Ticket 04 (`.scratch/apex-memory/`): the LSP server is the one
        // caller that opts into Pass 2 working-set scoping -- the CLI
        // (`apexls check`) and every test/batch caller of `BoundProgram`
        // never touch a `BindState` at all, so this is the single place
        // that turns it on, once, for the life of the connection.
        let mut cache = BindCache::default();
        cache.scoping_enabled = true;
        Self {
            program: RwLock::default(),
            cache: Mutex::new(cache),
            documents: Mutex::default(),
            bound_version: tokio::sync::watch::channel(0).0,
            worker_active: AtomicBool::new(false),
            rebuild_requested: tokio::sync::Notify::default(),
            progress_seq: AtomicU64::new(0),
            watcher: Mutex::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::panic::AssertUnwindSafe;

    /// The whole reason `BindState`'s locks are `parking_lot`'s rather
    /// than `std::sync`'s (see the doc comment on the struct itself): a
    /// panic while a guard is held must never leave the lock permanently
    /// unusable for the rest of the process. `std::sync::RwLock` would
    /// poison here, and every later `.read()`/`.write()` would panic too
    /// -- `parking_lot`'s never poisons, so the lock must still work
    /// completely normally right after.
    #[test]
    fn a_panic_while_holding_programs_write_lock_does_not_poison_it() {
        let bind = BindState::default();

        let panicked = std::panic::catch_unwind(AssertUnwindSafe(|| {
            let mut guard = bind.program.write();
            *guard = None;
            panic!("simulated panic while `program`'s write guard is held");
        }));
        assert!(panicked.is_err(), "the simulated panic should have actually unwound");

        // If this were `std::sync::RwLock`, both of these would now
        // panic too (poisoned forever). With `parking_lot`, they don't.
        assert!(bind.program.read().is_none());
        *bind.program.write() = None;
    }

    /// The three real shapes `sf apex run test --result-format json
    /// --json` actually produces (confirmed live against a connected org
    /// per the decision ticket -- these are exact captured payloads, not
    /// guessed): an all-passing run, a run with a failing test, and a
    /// CLI-level rejection before anything ran.
    #[test]
    fn summarize_apex_test_output_reports_a_passing_run_as_info() {
        let stdout = br#"{
            "status": 0,
            "result": {
                "summary": { "outcome": "Passed", "passing": 1, "failing": 0, "testsRan": 1 },
                "tests": [{ "FullName": "FooTest.testBar", "Outcome": "Pass", "Message": null }]
            },
            "warnings": []
        }"#;
        let (message_type, message) = summarize_apex_test_output("FooTest", stdout);
        assert_eq!(message_type, MessageType::INFO);
        assert!(message.contains("Passed"), "expected the outcome in the message: {message}");
        assert!(!message.contains("testBar"), "a passing run shouldn't list per-test detail");
    }

    #[test]
    fn summarize_apex_test_output_reports_a_failing_run_as_error_with_detail() {
        let stdout = br#"{
            "status": 100,
            "result": {
                "summary": { "outcome": "Failed", "passing": 0, "failing": 1, "testsRan": 1 },
                "tests": [{
                    "FullName": "LispValueTest.null",
                    "Outcome": "Fail",
                    "Message": "line 7, column 23: Variable is not visible: LispValue.TAG_FIXNUM"
                }]
            },
            "warnings": []
        }"#;
        let (message_type, message) = summarize_apex_test_output("LispValueTest", stdout);
        assert_eq!(message_type, MessageType::ERROR);
        assert!(message.contains("LispValueTest.null"), "expected the failing test named: {message}");
        assert!(
            message.contains("Variable is not visible"),
            "expected the failure message included: {message}"
        );
    }

    #[test]
    fn summarize_apex_test_output_reports_a_cli_level_rejection_as_error() {
        let stdout = br#"{
            "name": "INVALID_INPUT",
            "message": "This class name's value is invalid: Bogus. Provide the name of an Apex class that has test methods.",
            "exitCode": 1,
            "status": 1
        }"#;
        let (message_type, message) = summarize_apex_test_output("Bogus", stdout);
        assert_eq!(message_type, MessageType::ERROR);
        assert!(
            message.contains("This class name's value is invalid"),
            "expected the CLI's own rejection message surfaced: {message}"
        );
    }

    #[test]
    fn summarize_apex_test_output_reports_unparseable_stdout_as_error() {
        let (message_type, message) = summarize_apex_test_output("FooTest", b"not json");
        assert_eq!(message_type, MessageType::ERROR);
        assert!(message.starts_with("apexls: "), "expected an apexls-prefixed message: {message}");
    }
}

/// Every open buffer's in-memory text, plus a monotonically increasing
/// `version` -- see `BindState::documents`'s doc comment for why the two
/// live behind one shared lock.
#[derive(Default)]
struct Documents {
    version: u64,
    texts: HashMap<Url, String>,
}

/// Blocks a read-only request's future until the background rebuild
/// worker has published a `bind.program` reflecting at least
/// `target_version` of `bind.documents` -- see `BindState::bound_version`'s
/// doc comment for the race this closes. A no-op when the rebuild worker
/// was never spawned (`BindState::worker_active`'s doc comment).
async fn wait_for_rebuild(bind: &BindState, target_version: u64) {
    if !bind.worker_active.load(Ordering::SeqCst) {
        return;
    }
    let mut bound_version = bind.bound_version.subscribe();
    let _ = bound_version
        .wait_for(|&version| version >= target_version)
        .await;
}

impl Backend {
    /// Requests a background rebuild reflecting the current
    /// `self.bind.documents` (each open buffer's in-memory text
    /// overriding its on-disk content -- see
    /// `BoundProgram::from_files_with_overrides`'s doc comment for why
    /// that matters). Just sets `self.bind.rebuild_requested`'s permit;
    /// the actual work happens on `spawn_rebuild_worker`'s persistent
    /// background task, spawned once from `initialized`. A harmless no-op
    /// if that worker was never spawned (no resolved root -- single-file
    /// mode, or a request that raced ahead of `initialized`): the permit
    /// is simply never consumed.
    fn schedule_rebuild(&self) {
        self.bind.rebuild_requested.notify_one();
    }

    /// Spawns a background task that registers a debounced filesystem
    /// watcher on `root`, matching rust-analyzer's own approach
    /// (`vfs-notify`, also built on `notify`) rather than depending on
    /// the LSP client to register and reliably deliver
    /// `workspace/didChangeWatchedFiles` notifications -- client support
    /// for that is inconsistent across editors. Any create/remove/modify
    /// under `root` touching a file `apex_discover::is_relevant_path`
    /// cares about (`.cls`/`.trigger`/`.object-meta.xml`/`.field-meta.xml`)
    /// invalidates `bind.cache`'s cached directory walk
    /// (`BindCache::invalidate_discovery`) and triggers a rebuild --
    /// closing the "a file added/removed on disk but never opened in the
    /// editor isn't picked up" gap `BoundProgram::from_files_cached`'s
    /// own doc comment otherwise accepts as an honest v1 limit. Events
    /// are debounced (a 500ms quiet period) since a single save, `git`
    /// operation, or build script routinely fires several raw events in
    /// quick succession for what's conceptually one change.
    ///
    /// Registration itself (`Debouncer::watch`) is `spawn_blocking`ed
    /// rather than run inline -- measured at ~330ms against the real
    /// NPSP corpus (~1044 files), which used to sit directly in
    /// `initialize`'s response path (a real, measured regression to
    /// server startup latency this fixed). Fire-and-forget, called from
    /// `initialized` alongside `Self::schedule_rebuild`: nothing here
    /// needs the watch to be live by any particular point, only
    /// *eventually*, matching how relying on it at all already accepts
    /// "not instant" (see the honest race-window note on `BindState::watcher`'s
    /// doc comment). The callback below runs on a thread `notify` owns,
    /// not a `tokio`-managed one, but only ever calls
    /// `rebuild_requested.notify_one()` -- a plain, synchronous method
    /// needing no runtime handle at all -- so unlike an earlier version
    /// of this function, no `tokio::runtime::Handle` needs threading
    /// through to it.
    fn start_watcher(root: PathBuf, bind: Arc<BindState>) {
        tokio::task::spawn_blocking(move || {
            let watch_root = root.clone();
            let callback_bind = Arc::clone(&bind);
            let mut debouncer = match new_debouncer(
                Duration::from_millis(500),
                None,
                move |result: DebounceEventResult| {
                    let events = match result {
                        Ok(events) => events,
                        Err(errors) => {
                            for error in errors {
                                warn!(%error, "filesystem watcher error");
                            }
                            return;
                        }
                    };
                    let relevant = events
                        .iter()
                        .flat_map(|event| event.paths.iter())
                        .any(|path| apex_discover::is_relevant_path(path));
                    if !relevant {
                        return;
                    }
                    callback_bind.cache.lock().invalidate_discovery();
                    callback_bind.rebuild_requested.notify_one();
                },
            ) {
                Ok(debouncer) => debouncer,
                Err(error) => {
                    warn!(%error, "failed to create filesystem watcher");
                    return;
                }
            };
            if let Err(error) = debouncer.watch(&watch_root, RecursiveMode::Recursive) {
                warn!(%error, root = %watch_root.display(), "failed to start filesystem watcher");
                return;
            }
            *bind.watcher.lock() = Some(debouncer);
        });
    }
}

/// Spawns the single persistent background rebuild worker for the
/// session, run for as long as the connection lives. Loops forever:
/// wait for `bind.rebuild_requested`, run exactly one rebuild reflecting
/// whatever's currently in `bind.documents`, then go back to waiting --
/// see `BindState::rebuild_requested`'s doc comment for why a `Notify`
/// makes this coalesce a burst of rapid edits into one rebuild (run the
/// instant the worker is free, not after some fixed delay) rather than
/// running -- or racing -- one per edit.
///
/// This replaced an earlier design (a fresh `spawn_blocking` task per
/// edit, debounced by a timer, with the *previous* pending timer
/// cancelled by each new edit) after that design caused a real,
/// reproduced bug: `overrides` was captured before acquiring
/// `bind.cache`'s lock, so a rebuild fed an *older* snapshot of
/// `bind.documents` could still acquire the lock *after* a newer
/// rebuild already had, leaving `apex_binder::BindCache` internally
/// inconsistent -- observed as a `SymbolTable::get` index-out-of-bounds
/// panic that poisoned `bind.cache`'s `Mutex` and permanently broke
/// every later rebuild for the rest of the session (a poisoned
/// `std::sync::Mutex` never recovers on its own). That version's fix
/// added an explicit "capture `overrides` only after acquiring the
/// lock" ordering argument on top of the debounce. This design doesn't
/// need that argument at all: with exactly one task ever calling
/// `from_files_cached`, sequentially, in a plain loop, there is no
/// second rebuild for an out-of-order one to race against in the first
/// place -- correct by construction, not by a debounce timer narrowing
/// the window enough that the race rarely fires.
fn spawn_rebuild_worker(
    root: PathBuf,
    bind: Arc<BindState>,
    client: ClientSocket,
    encoding: PositionEncoding,
    work_done_progress_supported: bool,
) {
    tokio::spawn(async move {
        loop {
            bind.rebuild_requested.notified().await;
            let bind = Arc::clone(&bind);
            let root = root.clone();
            // `version`/`texts` come from the same lock acquisition --
            // see `BindState::documents`'s doc comment for why that
            // pairing has to be atomic: it's the only thing that lets
            // `bound_version` (published below) honestly describe what
            // `program` covers. Captured here, on this task, rather than
            // inside the `spawn_blocking` closure below: `version` needs
            // to survive into the `Err` arm past the closure, for exactly
            // the reason explained there.
            let (version, overrides) = {
                let documents = bind.documents.lock();
                let overrides: HashMap<PathBuf, String> = documents
                    .texts
                    .iter()
                    .filter_map(|(uri, text)| uri.to_file_path().ok().map(|path| (path, text.clone())))
                    .collect();
                (documents.version, overrides)
            };
            // Progress reporting runs as its own detached task, never
            // awaited by this loop: the actual rebuild below must never
            // wait on a client's `window/workDoneProgress/create`
            // round-trip before starting (or on its `end` progress
            // notification before publishing) -- an unresponsive client
            // would otherwise turn a UX nicety into a stalled rebuild
            // pipeline. This task independently begins progress, then
            // reuses `wait_for_rebuild` (the same mechanism every
            // request handler already uses) to learn when *this*
            // rebuild's `version` has published, and ends progress then.
            if work_done_progress_supported {
                let progress_bind = Arc::clone(&bind);
                let progress_client = client.clone();
                tokio::spawn(async move {
                    let Some(token) = begin_rebuild_progress(&progress_bind, &progress_client).await else {
                        return;
                    };
                    wait_for_rebuild(&progress_bind, version).await;
                    let _ = progress_client.notify::<lsp_types::notification::Progress>(ProgressParams {
                        token,
                        value: ProgressParamsValue::WorkDone(WorkDoneProgress::End(WorkDoneProgressEnd {
                            message: None,
                        })),
                    });
                });
            }
            let rebuild_bind = Arc::clone(&bind);
            let result = tokio::task::spawn_blocking(move || {
                let mut cache = rebuild_bind.cache.lock();
                // Ticket 04: every open buffer is unconditionally pinned in
                // the Pass 2 working set -- `overrides` (built above from
                // exactly the same `documents.texts` snapshot) already *is*
                // that set, keyed the same way `open_paths` needs to be.
                cache.open_paths = overrides.keys().cloned().collect();
                let program = BoundProgram::from_files_cached(&root, &overrides, &mut cache);
                drop(cache);
                let file_count = program.file_count();
                *rebuild_bind.program.write() = Some(program);
                file_count
            })
            .await;
            // `send_replace`, not `send`: `send` silently no-ops (doesn't
            // even store the value) whenever the channel has zero active
            // receivers -- and since every `wait_for_rebuild` caller's
            // `subscribe()`d `Receiver` is transient (dropped the moment
            // its wait resolves), the receiver count is back to zero
            // between requests far more often than not. `send_replace`
            // updates the value unconditionally, exactly the "publish
            // state for whoever looks next" semantics this needs.
            //
            // Published on the `Err` arm too (a panic inside
            // `from_files_cached`, e.g. `crates/apex-binder/src/symbol_table.rs`'s
            // now-fixed index-out-of-bounds, or any future one) --
            // `bind.program` simply keeps whatever it last held, matching
            // this server's existing "serve stale data rather than go
            // silent" degradation for a broken rebuild. Without this,
            // `wait_for_rebuild` would block forever on every request
            // from here on: nothing else ever moves `bound_version`
            // again for a version a panicked rebuild was the one attempt
            // at reflecting, turning one rebuild-worker bug into a
            // permanently wedged server instead of a stale-but-responsive
            // one -- a real regression this hit, live, the same day this
            // waiting mechanism shipped.
            match result {
                Ok(file_count) => {
                    bind.bound_version.send_replace(version);
                    info!(file_count, "rebuild complete");
                    // Only publish if `version` is still current: two
                    // `schedule_rebuild` calls that land back-to-back
                    // (e.g. `initialized`'s own unconditional call
                    // immediately followed by the first `did_open`) don't
                    // always coalesce into the single `Notify` permit the
                    // struct doc comment describes -- tokio's scheduler
                    // can poll this task's first `.notified().await` (via
                    // its LIFO-slot fast path for a just-spawned task)
                    // *before* a second, already-queued notification's own
                    // handler has run, so the second `notify_one()` still
                    // lands on an already-consumed permit and queues a
                    // real second loop iteration. Both rebuilds are
                    // otherwise correct (each reads a genuinely fresh
                    // `bind.documents` snapshot at its own wake time), but
                    // without this check their two `publish_diagnostics`
                    // calls race: if the *earlier*, now-superseded
                    // rebuild's publish lands after the later one's, the
                    // client's diagnostics silently regress to stale data
                    // -- observed live as `unknown_schema_diagnostics.rs`'s
                    // `fixing_a_bad_soql_from_object_clears_the_diagnostic`
                    // flaking. Skipping a stale publish never loses a
                    // diagnostic update: `documents.version` only ever
                    // moves forward via a mutation that itself calls
                    // `schedule_rebuild`, so a version bump past this
                    // rebuild's own guarantees at least one more loop
                    // iteration -- reflecting *that* version -- still to
                    // come.
                    if bind.documents.lock().version == version {
                        publish_diagnostics(&bind, &client, encoding);
                    }
                }
                Err(join_error) => {
                    bind.bound_version.send_replace(version);
                    warn!(%join_error, "rebuild panicked; serving the last successful bind");
                }
            }
        }
    });
}

/// Pushes a fresh `textDocument/publishDiagnostics` notification for
/// every currently-open document, right after each rebuild completes --
/// diagnostics are server-initiated (unlike hover/references/etc., which
/// answer a client request), so they can't be computed lazily behind
/// `wait_for_rebuild` the way every other capability is. Scoped to open
/// documents only (`bind.documents`'s own key set): recomputing for
/// every project file (~1000+ on the real NPSP corpus) on every
/// keystroke would be wasted work no client displays anyway -- every
/// real editor only shows diagnostics for buffers it has open. Published
/// unconditionally for every open file, including an empty
/// `diagnostics: vec![]` -- otherwise a dead symbol (or a fixed syntax
/// error) would leave its stale squiggle on screen forever, since nothing
/// else would ever tell the client to clear it.
///
/// Combines every diagnostic source (`capabilities::diagnostics_for_file`,
/// shared with the pull-model `Backend::document_diagnostic` below) into
/// *one* notification per file -- `textDocument/publishDiagnostics`
/// replaces a client's whole diagnostic set for a URI on every
/// notification rather than merging with the previous one, so sending two
/// separate notifications for the same file would make the second one
/// silently wipe out the first.
fn publish_diagnostics(bind: &BindState, client: &ClientSocket, encoding: PositionEncoding) {
    let program_guard = bind.program.read();
    let Some(program) = program_guard.as_ref() else {
        return;
    };
    let uris: Vec<Url> = bind.documents.lock().texts.keys().cloned().collect();
    for uri in uris {
        let Some(path) = uri.to_file_path().ok() else {
            continue;
        };
        let Some(file) = program.file_id(&path) else {
            continue;
        };
        let diagnostics = capabilities::diagnostics_for_file(program, file, encoding);
        let _ = client.notify::<lsp_types::notification::PublishDiagnostics>(PublishDiagnosticsParams {
            uri,
            diagnostics,
            version: None,
        });
    }
}

/// Starts one `$/progress` reporting cycle for a rebuild. Only called when
/// the client already declared `window.workDoneProgress` support at
/// `initialize` time (see `spawn_rebuild_worker`'s guard and
/// `Backend::work_done_progress_supported`'s doc comment) -- sending
/// progress notifications a client never asked for would just be
/// discarded. Per spec, a server must first ask the client to create a
/// token (`window/workDoneProgress/create`) before reporting against it;
/// a fresh token per rebuild (`BindState::progress_seq`) keeps each
/// rebuild's begin/end pair unambiguous to the client. Returns `None` if
/// the create request itself errors (e.g. the client claims support but
/// refuses), so the caller knows to skip sending `end` too -- doing so
/// for a token the client never agreed to create would be a protocol
/// violation, not just a wasted notification.
async fn begin_rebuild_progress(bind: &BindState, client: &ClientSocket) -> Option<NumberOrString> {
    let seq = bind.progress_seq.fetch_add(1, Ordering::SeqCst);
    let token = NumberOrString::String(format!("apexls/rebuild-{seq}"));
    client
        .request::<lsp_types::request::WorkDoneProgressCreate>(WorkDoneProgressCreateParams {
            token: token.clone(),
        })
        .await
        .ok()?;
    let _ = client.notify::<lsp_types::notification::Progress>(ProgressParams {
        token: token.clone(),
        value: ProgressParamsValue::WorkDone(WorkDoneProgress::Begin(WorkDoneProgressBegin {
            title: "apexls: rebuilding".into(),
            cancellable: Some(false),
            message: None,
            percentage: None,
        })),
    });
    Some(token)
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
        let work_done_progress_supported = params
            .capabilities
            .window
            .as_ref()
            .and_then(|w| w.work_done_progress)
            .unwrap_or(false);

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
        self.work_done_progress_supported = work_done_progress_supported;
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
                    // `,` re-triggers signature help on every new argument,
                    // not just `(` on the call's opening paren.
                    signature_help_provider: Some(SignatureHelpOptions {
                        trigger_characters: Some(vec!["(".into(), ",".into()]),
                        retrigger_characters: None,
                        work_done_progress_options: Default::default(),
                    }),
                    definition_provider: Some(OneOf::Left(true)),
                    references_provider: Some(OneOf::Left(true)),
                    document_highlight_provider: Some(OneOf::Left(true)),
                    // `prepare_provider: true` -- the client always sends
                    // `textDocument/prepareRename` first to get a range/
                    // validity check before showing its rename input box,
                    // rather than only finding out a target was refused
                    // (an ambiguous overload, an override-chain method,
                    // ...) after the user already typed a new name.
                    rename_provider: Some(OneOf::Right(RenameOptions {
                        prepare_provider: Some(true),
                        work_done_progress_options: Default::default(),
                    })),
                    document_symbol_provider: Some(OneOf::Left(true)),
                    workspace_symbol_provider: Some(OneOf::Left(true)),
                    folding_range_provider: Some(FoldingRangeProviderCapability::Simple(true)),
                    selection_range_provider: Some(SelectionRangeProviderCapability::Simple(true)),
                    // Legend reused verbatim from `capabilities::LEGEND_TYPES`/
                    // `LEGEND_MODIFIERS` -- see that module's doc comment for
                    // why those arrays are this legend's single source of
                    // truth. `full: Bool(true)`, not `Delta`: delta encoding
                    // (`textDocument/semanticTokens/full/delta`) isn't
                    // implemented in v1 -- the walker's output is already
                    // sorted/deduped, so a diff between two runs would be
                    // mechanical to add if a real client (or a real perf
                    // number) ever asks for it. `range: true` because a
                    // range request is the same walk filtered by
                    // `TextRange::contains_range`, materially cheaper than
                    // always producing the whole file's tokens when a client
                    // only wants the visible viewport.
                    semantic_tokens_provider: Some(SemanticTokensServerCapabilities::SemanticTokensOptions(
                        SemanticTokensOptions {
                            work_done_progress_options: Default::default(),
                            legend: SemanticTokensLegend {
                                token_types: capabilities::LEGEND_TYPES.to_vec(),
                                token_modifiers: capabilities::LEGEND_MODIFIERS.to_vec(),
                            },
                            range: Some(true),
                            full: Some(SemanticTokensFullOptions::Bool(true)),
                        },
                    )),
                    // `QUICKFIX` (`capabilities::dead_code_actions`) and
                    // `REFACTOR_REWRITE` (`capabilities::parameter_reorder_actions`)
                    // -- the only two kinds `code_action` ever returns today.
                    code_action_provider: Some(CodeActionProviderCapability::Options(
                        CodeActionOptions {
                            code_action_kinds: Some(vec![
                                CodeActionKind::QUICKFIX,
                                CodeActionKind::REFACTOR_REWRITE,
                            ]),
                            ..Default::default()
                        },
                    )),
                    call_hierarchy_provider: Some(CallHierarchyServerCapability::Options(
                        CallHierarchyOptions {
                            work_done_progress_options: Default::default(),
                        },
                    )),
                    inlay_hint_provider: Some(OneOf::Left(true)),
                    // `.` triggers completion for a member access; a bare
                    // identifier needs no trigger character at all -- every
                    // real client already invokes completion as the user
                    // types ordinary word characters regardless of what's
                    // listed here.
                    completion_provider: Some(CompletionOptions {
                        trigger_characters: Some(vec![".".into()]),
                        resolve_provider: Some(false),
                        ..Default::default()
                    }),
                    // Pull diagnostics (`Backend::document_diagnostic`),
                    // alongside the existing unconditional push
                    // (`publish_diagnostics`) -- both read the exact same
                    // `capabilities::diagnostics_for_file` merge, so
                    // advertising this costs nothing a client couldn't
                    // already get, it just lets a client (or another tool
                    // driving apexls, e.g. an agent) ask synchronously
                    // instead of waiting on the next push. No caching: no
                    // `identifier`, and every response is a fresh `Full`
                    // report rather than ever using `previous_result_id`
                    // to answer `Unchanged` -- the merge is cheap enough
                    // (same cost `publish_diagnostics` already pays per
                    // rebuild) that result-id bookkeeping isn't earning
                    // its complexity yet. `inter_file_dependencies: true`
                    // because Apex resolution genuinely is cross-file
                    // (e.g. `missing_implementation_diagnostics`/
                    // `type_mismatch_diagnostics` depend on another
                    // file's declarations) -- `false` here would be a
                    // lie a client could reasonably rely on.
                    // `workspace_diagnostics: false`: `workspace/diagnostic`
                    // itself isn't implemented.
                    diagnostic_provider: Some(DiagnosticServerCapabilities::Options(
                        DiagnosticOptions {
                            identifier: None,
                            inter_file_dependencies: true,
                            workspace_diagnostics: false,
                            work_done_progress_options: Default::default(),
                        },
                    )),
                    // Run Test lens (ticket 02/03,
                    // `.scratch/apex-lsp-gaps/issues/02-run-test-lens-decision.md`).
                    // `resolve_provider: None` -- `capabilities::run_test_lenses`
                    // already computes each lens's full `Command` eagerly,
                    // no separate `codeLens/resolve` round-trip needed.
                    code_lens_provider: Some(CodeLensOptions {
                        resolve_provider: None,
                    }),
                    // `apexls.runTest` is the lens's own command (see
                    // `Backend::execute_command`) -- server-side, not a
                    // client-owned command id, so the lens works in any
                    // LSP client without a matching editor extension.
                    execute_command_provider: Some(ExecuteCommandOptions {
                        commands: vec!["apexls.runTest".into()],
                        work_done_progress_options: Default::default(),
                    }),
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
        if let Some(root) = self.root.as_ref().and_then(|url| url.to_file_path().ok()) {
            // Set before spawning, not after: `wait_for_rebuild` needs to
            // see this as `true` for every request dispatched from here
            // on, and `initialized` (a notification) is guaranteed to
            // finish before any later request is even dispatched -- see
            // this module's own doc comment on notification-before-
            // request ordering.
            self.bind.worker_active.store(true, Ordering::SeqCst);
            spawn_rebuild_worker(
                root.clone(),
                Arc::clone(&self.bind),
                self.client.clone(),
                self.position_encoding,
                self.work_done_progress_supported,
            );
            Backend::start_watcher(root, Arc::clone(&self.bind));
        }
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
        let mut documents = self.bind.documents.lock();
        documents.texts.insert(uri, params.text_document.text);
        documents.version += 1;
        drop(documents);
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
        let mut documents = self.bind.documents.lock();
        documents.texts.insert(uri, change.text);
        documents.version += 1;
        drop(documents);
        self.schedule_rebuild();
        ControlFlow::Continue(())
    }

    fn did_close(&mut self, params: DidCloseTextDocumentParams) -> Self::NotifyResult {
        let uri = params.text_document.uri;
        info!(%uri, "did_close");
        let mut documents = self.bind.documents.lock();
        documents.texts.remove(&uri);
        documents.version += 1;
        drop(documents);
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
    /// rather than silently picking one; `StdlibMember`/`Label`/
    /// `VisualforcePage` render via their own describe functions; `SchemaObject`/`UnknownSchema`/
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
        let target_version = self.bind.documents.lock().version;
        let bind = Arc::clone(&self.bind);
        Box::pin(async move {
            wait_for_rebuild(&bind, target_version).await;
            let program_guard = bind.program.read();
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
                    Some(Resolution::StdlibMember(r)) => capabilities::describe_stdlib_member(program, r),
                    Some(Resolution::Label(r)) => capabilities::describe_label(program, r),
                    Some(Resolution::VisualforcePage(r)) => capabilities::describe_visualforce_page(program, r),
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

    /// `capabilities::signature_help`: which overload(s) of the call the
    /// cursor sits inside of could apply, and which parameter position
    /// it's currently in. `params.context` (which trigger character
    /// fired, whether a signature help popup was already open) isn't
    /// consulted -- every trigger recomputes the same on-demand answer
    /// from scratch, cheap enough (a handful of `SymbolTable` lookups)
    /// that there's no real benefit to threading the previous popup's
    /// state through.
    fn signature_help(
        &mut self,
        params: SignatureHelpParams,
    ) -> BoxFuture<'static, Result<Option<SignatureHelp>, Self::Error>> {
        let uri = params.text_document_position_params.text_document.uri;
        let position = params.text_document_position_params.position;
        let encoding = self.position_encoding;
        let target_version = self.bind.documents.lock().version;
        let bind = Arc::clone(&self.bind);
        Box::pin(async move {
            wait_for_rebuild(&bind, target_version).await;
            let program_guard = bind.program.read();
            let Some(program) = program_guard.as_ref() else {
                return Ok(None);
            };
            let Some((file, offset)) =
                capabilities::resolve_position(program, &uri, position, encoding)
            else {
                return Ok(None);
            };

            Ok(capabilities::signature_help(program, file, offset))
        })
    }

    /// `capabilities::completion`: every candidate for whatever context
    /// (member-access after a `.`, or a bare identifier) the cursor sits
    /// in. `params.context` (which trigger character fired, if any)
    /// isn't consulted -- same on-demand posture as `signature_help`,
    /// nothing here is expensive enough to need it.
    fn completion(
        &mut self,
        params: CompletionParams,
    ) -> BoxFuture<'static, Result<Option<CompletionResponse>, Self::Error>> {
        let uri = params.text_document_position.text_document.uri;
        let position = params.text_document_position.position;
        let encoding = self.position_encoding;
        let target_version = self.bind.documents.lock().version;
        let bind = Arc::clone(&self.bind);
        Box::pin(async move {
            wait_for_rebuild(&bind, target_version).await;
            let program_guard = bind.program.read();
            let Some(program) = program_guard.as_ref() else {
                return Ok(None);
            };
            let Some((file, offset)) =
                capabilities::resolve_position(program, &uri, position, encoding)
            else {
                return Ok(None);
            };

            Ok(capabilities::completion(program, file, offset, encoding))
        })
    }

    /// `capabilities::inlay_hints`: a `paramName:` label before each
    /// call argument visible in `params.range` -- see that function's
    /// own doc comment for exactly which calls get a label and which
    /// don't.
    fn inlay_hint(
        &mut self,
        params: InlayHintParams,
    ) -> BoxFuture<'static, Result<Option<Vec<InlayHint>>, Self::Error>> {
        let uri = params.text_document.uri;
        let range = params.range;
        let encoding = self.position_encoding;
        let target_version = self.bind.documents.lock().version;
        let bind = Arc::clone(&self.bind);
        Box::pin(async move {
            wait_for_rebuild(&bind, target_version).await;
            let program_guard = bind.program.read();
            let Some(program) = program_guard.as_ref() else {
                return Ok(None);
            };
            Ok(capabilities::inlay_hints(program, &uri, range, encoding))
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
        let target_version = self.bind.documents.lock().version;
        let bind = Arc::clone(&self.bind);
        Box::pin(async move {
            wait_for_rebuild(&bind, target_version).await;
            let program_guard = bind.program.read();
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
                Some(Resolution::SchemaObject(r)) => {
                    capabilities::schema_location(program, r).map(GotoDefinitionResponse::Scalar)
                }
                Some(Resolution::Label(r)) => {
                    capabilities::label_location(program, r).map(GotoDefinitionResponse::Scalar)
                }
                Some(Resolution::VisualforcePage(r)) => {
                    capabilities::visualforce_page_location(program, r).map(GotoDefinitionResponse::Scalar)
                }
                _ => None,
            };

            Ok(response)
        })
    }

    /// `capabilities::references`: every location project-wide
    /// referencing the symbol at the cursor (a declaration or a
    /// reference), via `BoundProgram::references_to`'s reverse-index
    /// lookup -- see `BACKLOG.md` §3. Genuinely needs every file's
    /// `bodies` at once, so this is one of the few capabilities that
    /// triggers ticket 04's (`.scratch/apex-memory/`) temporary
    /// full-project spike-bind (`BoundProgram::with_full_binding`) rather
    /// than reading the working-set-scoped snapshot as-is -- run on
    /// `spawn_blocking` like `spawn_rebuild_worker`'s own rebuilds, since
    /// binding every still-unbound file is real CPU work that must never
    /// run inline on an async worker thread while holding `bind.program`'s
    /// write lock (that would stall the whole runtime for every other
    /// in-flight request, not just this one).
    fn references(
        &mut self,
        params: ReferenceParams,
    ) -> BoxFuture<'static, Result<Option<Vec<Location>>, Self::Error>> {
        let uri = params.text_document_position.text_document.uri;
        let position = params.text_document_position.position;
        let include_declaration = params.context.include_declaration;
        let encoding = self.position_encoding;
        let target_version = self.bind.documents.lock().version;
        let bind = Arc::clone(&self.bind);
        Box::pin(async move {
            wait_for_rebuild(&bind, target_version).await;
            let locations = tokio::task::spawn_blocking(move || {
                let mut program_guard = bind.program.write();
                let Some(program) = program_guard.as_mut() else {
                    return Vec::new();
                };
                let Some((file, offset)) =
                    capabilities::resolve_position(program, &uri, position, encoding)
                else {
                    return Vec::new();
                };
                let mut cache = bind.cache.lock();
                program.with_full_binding(&mut cache, |program| {
                    capabilities::references(program, file, offset, include_declaration, encoding)
                })
            })
            .await
            .unwrap_or_default();
            Ok((!locations.is_empty()).then_some(locations))
        })
    }

    /// `capabilities::document_highlights`: every occurrence of the
    /// symbol at the cursor, scoped to this one file.
    fn document_highlight(
        &mut self,
        params: DocumentHighlightParams,
    ) -> BoxFuture<'static, Result<Option<Vec<DocumentHighlight>>, Self::Error>> {
        let uri = params.text_document_position_params.text_document.uri;
        let position = params.text_document_position_params.position;
        let encoding = self.position_encoding;
        let target_version = self.bind.documents.lock().version;
        let bind = Arc::clone(&self.bind);
        Box::pin(async move {
            wait_for_rebuild(&bind, target_version).await;
            let program_guard = bind.program.read();
            let Some(program) = program_guard.as_ref() else {
                return Ok(None);
            };
            let Some((file, offset)) =
                capabilities::resolve_position(program, &uri, position, encoding)
            else {
                return Ok(None);
            };

            let highlights = capabilities::document_highlights(program, file, offset, encoding);
            Ok((!highlights.is_empty()).then_some(highlights))
        })
    }

    /// `capabilities::prepare_rename_range`, gated by `capabilities::rename_target`'s
    /// full eligibility check (ambiguous resolution, an override-chain
    /// method, a trigger, ...) -- refuses with a `ResponseError` rather
    /// than silently returning `None`, so the client shows the user
    /// *why* rename isn't offered here instead of just not offering it.
    /// `rename_target` itself needs every file's `bodies`
    /// (`references_resolve_cleanly` -> `references_to`), so this also
    /// goes through ticket 04's (`.scratch/apex-memory/`) full-project
    /// spike-bind, same as `references`/`rename` -- a client is free to
    /// call `prepareRename` without ever calling `rename` after, so the
    /// eligibility check here can't defer that cost to the later request.
    /// Run on `spawn_blocking`, same reasoning as `references`.
    fn prepare_rename(
        &mut self,
        params: TextDocumentPositionParams,
    ) -> BoxFuture<'static, Result<Option<PrepareRenameResponse>, Self::Error>> {
        let uri = params.text_document.uri;
        let position = params.position;
        let encoding = self.position_encoding;
        let target_version = self.bind.documents.lock().version;
        let bind = Arc::clone(&self.bind);
        Box::pin(async move {
            wait_for_rebuild(&bind, target_version).await;
            tokio::task::spawn_blocking(move || {
                let mut program_guard = bind.program.write();
                let Some(program) = program_guard.as_mut() else {
                    return Ok(None);
                };
                let Some((file, offset)) =
                    capabilities::resolve_position(program, &uri, position, encoding)
                else {
                    return Ok(None);
                };

                let mut cache = bind.cache.lock();
                program.with_full_binding(&mut cache, |program| {
                    if let Err(refusal) = capabilities::rename_target(program, file, offset) {
                        return Err(ResponseError::new(ErrorCode::REQUEST_FAILED, refusal.message()));
                    }
                    let range = capabilities::prepare_rename_range(program, file, offset, encoding);
                    Ok(range.map(PrepareRenameResponse::Range))
                })
            })
            .await
            .unwrap_or_else(|_| {
                Err(ResponseError::new(ErrorCode::REQUEST_FAILED, "internal error: bind task panicked"))
            })
        })
    }

    /// `capabilities::rename_edits`: a project-wide `WorkspaceEdit`
    /// renaming the symbol at the cursor, built from exactly the same
    /// `references_to`/`highlight_range` data `references` already uses --
    /// so this too goes through ticket 04's (`.scratch/apex-memory/`)
    /// full-project spike-bind. Refuses (a `ResponseError`, never a silent
    /// empty edit) for anything `capabilities::rename_target`'s
    /// eligibility check wouldn't have offered via `prepareRename` either,
    /// since a client is allowed to skip `prepareRename` and call this
    /// directly. Run on `spawn_blocking`, same reasoning as `references`.
    fn rename(
        &mut self,
        params: RenameParams,
    ) -> BoxFuture<'static, Result<Option<WorkspaceEdit>, Self::Error>> {
        let uri = params.text_document_position.text_document.uri;
        let position = params.text_document_position.position;
        let new_name = params.new_name;
        let encoding = self.position_encoding;
        let target_version = self.bind.documents.lock().version;
        let bind = Arc::clone(&self.bind);
        Box::pin(async move {
            wait_for_rebuild(&bind, target_version).await;
            tokio::task::spawn_blocking(move || {
                let mut program_guard = bind.program.write();
                let Some(program) = program_guard.as_mut() else {
                    return Ok(None);
                };
                let Some((file, offset)) =
                    capabilities::resolve_position(program, &uri, position, encoding)
                else {
                    return Ok(None);
                };

                let mut cache = bind.cache.lock();
                program.with_full_binding(&mut cache, |program| {
                    match capabilities::rename_edits(program, file, offset, &new_name, encoding) {
                        Ok(edit) => Ok(Some(edit)),
                        Err(refusal) => {
                            Err(ResponseError::new(ErrorCode::REQUEST_FAILED, refusal.message()))
                        }
                    }
                })
            })
            .await
            .unwrap_or_else(|_| {
                Err(ResponseError::new(ErrorCode::REQUEST_FAILED, "internal error: bind task panicked"))
            })
        })
    }

    /// `capabilities::prepare_call_hierarchy`: the callable(s) at the
    /// cursor, the first leg of the three-request call-hierarchy dance
    /// (prepare, then the client asks `incomingCalls`/`outgoingCalls`
    /// per item it wants to expand).
    fn prepare_call_hierarchy(
        &mut self,
        params: CallHierarchyPrepareParams,
    ) -> BoxFuture<'static, Result<Option<Vec<CallHierarchyItem>>, Self::Error>> {
        let uri = params.text_document_position_params.text_document.uri;
        let position = params.text_document_position_params.position;
        let encoding = self.position_encoding;
        let target_version = self.bind.documents.lock().version;
        let bind = Arc::clone(&self.bind);
        Box::pin(async move {
            wait_for_rebuild(&bind, target_version).await;
            let program_guard = bind.program.read();
            let Some(program) = program_guard.as_ref() else {
                return Ok(None);
            };
            let Some((file, offset)) =
                capabilities::resolve_position(program, &uri, position, encoding)
            else {
                return Ok(None);
            };

            let items = capabilities::prepare_call_hierarchy(program, file, offset, encoding);
            Ok((!items.is_empty()).then_some(items))
        })
    }

    /// `capabilities::incoming_calls`: every caller of the callable
    /// `params.item` names, re-resolved from its own `uri`/
    /// `selection_range` against whatever bind is live right now.
    /// Genuinely project-wide (`call_hierarchy::incoming_calls` ->
    /// `BoundProgram::references_to`, unlike `outgoing_calls`/
    /// `prepare_call_hierarchy`, which only ever need one file's own
    /// `bodies`) -- ticket 04's (`.scratch/apex-memory/`) own research
    /// missed this one, but it needs exactly the same full-project
    /// spike-bind treatment as `references`/`rename`, including running on
    /// `spawn_blocking`.
    fn incoming_calls(
        &mut self,
        params: CallHierarchyIncomingCallsParams,
    ) -> BoxFuture<'static, Result<Option<Vec<CallHierarchyIncomingCall>>, Self::Error>> {
        let uri = params.item.uri;
        let position = params.item.selection_range.start;
        let encoding = self.position_encoding;
        let target_version = self.bind.documents.lock().version;
        let bind = Arc::clone(&self.bind);
        Box::pin(async move {
            wait_for_rebuild(&bind, target_version).await;
            let calls = tokio::task::spawn_blocking(move || {
                let mut program_guard = bind.program.write();
                let Some(program) = program_guard.as_mut() else {
                    return None;
                };
                let Some((file, offset)) =
                    capabilities::resolve_position(program, &uri, position, encoding)
                else {
                    return None;
                };

                let mut cache = bind.cache.lock();
                program.with_full_binding(&mut cache, |program| {
                    capabilities::incoming_calls(program, file, offset, encoding)
                })
            })
            .await
            .unwrap_or_default();
            Ok(calls)
        })
    }

    /// `capabilities::outgoing_calls`: the mirror image of `incoming_calls`.
    /// Unlike every other position-based capability, `params.item.uri`
    /// here names whatever file a call-hierarchy item was expanded from --
    /// not necessarily an open document -- so `outgoing_calls` (needing
    /// only that one file's own `bodies`, via `BoundProgram::call_sites_in_range`)
    /// promotes it on demand (`BoundProgram::ensure_bound`, ticket 04)
    /// rather than assuming it's already Pass-2-bound the way an open
    /// document always is. Tries a read-only fast path first (the common
    /// case: `file` is already bound), falling back to a *single* write-
    /// lock scope that resolves the position, promotes, and answers all
    /// together -- resolving under one lock and promoting under another,
    /// separately acquired one, would leave a window for a concurrent
    /// background rebuild to land in between and publish a fresh snapshot
    /// that never got the promotion, silently losing it for this request.
    fn outgoing_calls(
        &mut self,
        params: CallHierarchyOutgoingCallsParams,
    ) -> BoxFuture<'static, Result<Option<Vec<CallHierarchyOutgoingCall>>, Self::Error>> {
        let uri = params.item.uri;
        let position = params.item.selection_range.start;
        let encoding = self.position_encoding;
        let target_version = self.bind.documents.lock().version;
        let bind = Arc::clone(&self.bind);
        Box::pin(async move {
            wait_for_rebuild(&bind, target_version).await;
            {
                let program_guard = bind.program.read();
                if let Some(program) = program_guard.as_ref() {
                    if let Some((file, offset)) =
                        capabilities::resolve_position(program, &uri, position, encoding)
                    {
                        if program.is_bound(file) {
                            return Ok(capabilities::outgoing_calls(program, file, offset, encoding));
                        }
                    }
                }
            }

            let mut program_guard = bind.program.write();
            let Some(program) = program_guard.as_mut() else {
                return Ok(None);
            };
            let Some((file, offset)) =
                capabilities::resolve_position(program, &uri, position, encoding)
            else {
                return Ok(None);
            };
            program.ensure_bound(file, &mut bind.cache.lock());
            Ok(capabilities::outgoing_calls(program, file, offset, encoding))
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
        let target_version = self.bind.documents.lock().version;
        let bind = Arc::clone(&self.bind);
        Box::pin(async move {
            wait_for_rebuild(&bind, target_version).await;
            let program_guard = bind.program.read();
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

    /// The pull-model counterpart to `publish_diagnostics`: a client (or
    /// another tool driving apexls) can ask for one file's diagnostics
    /// synchronously instead of waiting for the next background-rebuild
    /// push. Shares `capabilities::diagnostics_for_file` with the push
    /// path so the two transports can never disagree about what a file's
    /// diagnostics are. Always answers with a fresh `Full` report --
    /// `previous_result_id`/`Unchanged` caching isn't implemented (see
    /// `diagnostic_provider`'s own doc comment in `initialize`) -- and an
    /// unknown/not-yet-bound file gets an empty `Full` report (`items:
    /// vec![]`) rather than an error, the same "no diagnostics known yet"
    /// default `publish_diagnostics` itself uses by skipping such files.
    fn document_diagnostic(
        &mut self,
        params: DocumentDiagnosticParams,
    ) -> BoxFuture<'static, Result<DocumentDiagnosticReportResult, Self::Error>> {
        let uri = params.text_document.uri;
        let encoding = self.position_encoding;
        let target_version = self.bind.documents.lock().version;
        let bind = Arc::clone(&self.bind);
        Box::pin(async move {
            wait_for_rebuild(&bind, target_version).await;
            let program_guard = bind.program.read();
            let items = program_guard
                .as_ref()
                .and_then(|program| {
                    let path = uri.to_file_path().ok()?;
                    let file = program.file_id(&path)?;
                    Some(capabilities::diagnostics_for_file(program, file, encoding))
                })
                .unwrap_or_default();
            Ok(DocumentDiagnosticReportResult::Report(DocumentDiagnosticReport::Full(
                RelatedFullDocumentDiagnosticReport {
                    related_documents: None,
                    full_document_diagnostic_report: FullDocumentDiagnosticReport {
                        result_id: None,
                        items,
                    },
                },
            )))
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
        let target_version = self.bind.documents.lock().version;
        let bind = Arc::clone(&self.bind);
        Box::pin(async move {
            wait_for_rebuild(&bind, target_version).await;
            let program_guard = bind.program.read();
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
        let target_version = self.bind.documents.lock().version;
        let bind = Arc::clone(&self.bind);
        Box::pin(async move {
            wait_for_rebuild(&bind, target_version).await;
            let program_guard = bind.program.read();
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
        let target_version = self.bind.documents.lock().version;
        let bind = Arc::clone(&self.bind);
        Box::pin(async move {
            wait_for_rebuild(&bind, target_version).await;
            let program_guard = bind.program.read();
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

    /// The whole file's semantic tokens (`capabilities::semantic_tokens_full`).
    /// `None` for a file the bind doesn't know about, matching every other
    /// handler's own "unbound file" behavior rather than an empty array --
    /// the two mean different things to a client (nothing to highlight vs.
    /// this server has no opinion yet).
    fn semantic_tokens_full(
        &mut self,
        params: SemanticTokensParams,
    ) -> BoxFuture<'static, Result<Option<SemanticTokensResult>, Self::Error>> {
        let uri = params.text_document.uri;
        let encoding = self.position_encoding;
        let target_version = self.bind.documents.lock().version;
        let bind = Arc::clone(&self.bind);
        Box::pin(async move {
            wait_for_rebuild(&bind, target_version).await;
            let program_guard = bind.program.read();
            let Some(program) = program_guard.as_ref() else {
                return Ok(None);
            };
            let Some(path) = uri.to_file_path().ok() else {
                return Ok(None);
            };
            let Some(file) = program.file_id(&path) else {
                return Ok(None);
            };
            Ok(Some(SemanticTokensResult::Tokens(capabilities::semantic_tokens_full(
                program, file, encoding,
            ))))
        })
    }

    /// `params.range`'s tokens only (`capabilities::semantic_tokens_range`).
    /// A `range` that no longer fits `file`'s current text (a stale request
    /// against a since-shortened file) resolves no tokens rather than
    /// erroring -- same defensive shape `resolve_position` already uses
    /// elsewhere for a position past the end of a file.
    fn semantic_tokens_range(
        &mut self,
        params: SemanticTokensRangeParams,
    ) -> BoxFuture<'static, Result<Option<SemanticTokensRangeResult>, Self::Error>> {
        let uri = params.text_document.uri;
        let lsp_range = params.range;
        let encoding = self.position_encoding;
        let target_version = self.bind.documents.lock().version;
        let bind = Arc::clone(&self.bind);
        Box::pin(async move {
            wait_for_rebuild(&bind, target_version).await;
            let program_guard = bind.program.read();
            let Some(program) = program_guard.as_ref() else {
                return Ok(None);
            };
            let Some(path) = uri.to_file_path().ok() else {
                return Ok(None);
            };
            let Some(file) = program.file_id(&path) else {
                return Ok(None);
            };
            let text = program.source_text(file);
            let index = LineIndex::new(&text);
            let Some(start) = index.to_offset(&text, lsp_range.start, encoding) else {
                return Ok(None);
            };
            let Some(end) = index.to_offset(&text, lsp_range.end, encoding) else {
                return Ok(None);
            };
            let range = TextRange::new(TextSize::from(start), TextSize::from(end));
            Ok(Some(SemanticTokensRangeResult::Tokens(capabilities::semantic_tokens_range(
                program, file, range, encoding,
            ))))
        })
    }

    /// `capabilities::dead_code_actions`: a "Remove unused ..." quick-fix
    /// for every dead-code diagnostic (`capabilities::dead_code_diagnostics`,
    /// published proactively after each rebuild -- see
    /// `publish_diagnostics`) whose symbol overlaps the
    /// requested range. Re-derives dead symbols from `program` itself
    /// rather than trusting `params.context.diagnostics`, so this works
    /// even for a client that requests code actions without having first
    /// displayed/round-tripped the diagnostic. Also offers
    /// `capabilities::parameter_reorder_actions` ("Rotate parameters
    /// left/right"/"Remove parameter ...") when the requested range lands
    /// on a non-virtual, non-overloaded method's own parameter -- see that
    /// function's own doc comment for exactly which methods qualify.
    fn code_action(
        &mut self,
        params: CodeActionParams,
    ) -> BoxFuture<'static, Result<Option<CodeActionResponse>, Self::Error>> {
        let uri = params.text_document.uri;
        let range = params.range;
        let encoding = self.position_encoding;
        let target_version = self.bind.documents.lock().version;
        let bind = Arc::clone(&self.bind);
        Box::pin(async move {
            wait_for_rebuild(&bind, target_version).await;
            let program_guard = bind.program.read();
            let Some(program) = program_guard.as_ref() else {
                return Ok(None);
            };
            let Some(path) = uri.to_file_path().ok() else {
                return Ok(None);
            };
            let Some(file) = program.file_id(&path) else {
                return Ok(None);
            };
            let mut actions = capabilities::dead_code_actions(program, file, range, encoding);
            actions.extend(capabilities::parameter_reorder_actions(
                program, file, range, encoding,
            ));
            Ok((!actions.is_empty()).then_some(actions))
        })
    }

    /// Run Test lens (ticket 02/03). Same snapshot + `wait_for_rebuild`
    /// shape every other capability uses -- `capabilities::run_test_lenses`
    /// is a pure read over the already-bound `SymbolTable`, no `sf`
    /// invocation happens here (that's `execute_command`, only once the
    /// user actually clicks a lens).
    fn code_lens(
        &mut self,
        params: CodeLensParams,
    ) -> BoxFuture<'static, Result<Option<Vec<CodeLens>>, Self::Error>> {
        let uri = params.text_document.uri;
        let encoding = self.position_encoding;
        let target_version = self.bind.documents.lock().version;
        let bind = Arc::clone(&self.bind);
        Box::pin(async move {
            wait_for_rebuild(&bind, target_version).await;
            let program_guard = bind.program.read();
            let Some(program) = program_guard.as_ref() else {
                return Ok(None);
            };
            let Some(path) = uri.to_file_path().ok() else {
                return Ok(None);
            };
            let Some(file) = program.file_id(&path) else {
                return Ok(None);
            };
            let lenses = capabilities::run_test_lenses(program, file, encoding);
            Ok((!lenses.is_empty()).then_some(lenses))
        })
    }

    /// `apexls.runTest` (the Run Test lens's own command, ticket 02's
    /// decision): shells out to `sf apex run test` itself rather than
    /// naming a client-owned command, so the lens works in any LSP
    /// client with no editor-extension glue. Never passes `-o`/
    /// `--target-org` -- `sf` resolves its own already-authenticated
    /// default org exactly as it would from a terminal, so apexls needs
    /// no org-config logic of its own (see the decision ticket's "Org
    /// resolution" section for why this was confirmed safe rather than
    /// assumed). Any other `command` name answers `Ok(None)` untouched --
    /// `apexls.runTest` is the only command this server ever registers.
    fn execute_command(
        &mut self,
        params: ExecuteCommandParams,
    ) -> BoxFuture<'static, Result<Option<serde_json::Value>, Self::Error>> {
        if params.command != "apexls.runTest" {
            return Box::pin(async move { Ok(None) });
        }
        let mut args = params.arguments.into_iter();
        let Some(class_name) = args.next().and_then(|v| v.as_str().map(str::to_owned)) else {
            return Box::pin(async move { Ok(None) });
        };
        let method_name = args.next().and_then(|v| v.as_str().map(str::to_owned));
        let client = self.client.clone();
        // `sf` resolves both the SFDX project (`sfdx-project.json`) and
        // its own default-org config relative to its current directory --
        // an LSP client's own process cwd is not guaranteed to be (and
        // usually isn't) the workspace root, so `sf` must be run from
        // `self.root` explicitly rather than inheriting whatever cwd
        // apexls itself happened to be launched from.
        let root = self.root.as_ref().and_then(|url| url.to_file_path().ok());
        Box::pin(async move {
            run_apex_test(&client, root.as_deref(), &class_name, method_name.as_deref()).await;
            Ok(None)
        })
    }
}

/// Runs `sf apex run test --tests <target>` for the Run Test lens
/// (`target` is `class_name` alone for a class-level lens, or
/// `class_name.method_name` for a method-level one -- see
/// `capabilities::run_test_lenses`) and surfaces the outcome via
/// `window/showMessage`. `tokio::process::Command`'s own `.wait_with_output().await`
/// doesn't block the executor (it's not CPU-bound work, unlike the
/// rebuild worker's `spawn_blocking` calls), so other requests keep
/// being serviced while a test run is in flight.
///
/// Both `--result-format json` (the test result itself) and the
/// top-level `--json` (the CLI's own success/error envelope) are passed
/// together: confirmed live against a connected org (see the decision
/// ticket) that this combination gives one uniform top-level shape for
/// both an executed run (`{"status", "result": {"summary", "tests"},
/// "warnings"}`) and a CLI-level rejection before anything ran
/// (`{"status", "message", "name", ...}`, no `result` key) -- so parsing
/// never needs a text-scraping fallback for the error case. `-o`/
/// `--target-org` is deliberately never passed (see `execute_command`'s
/// doc comment). `--synchronous` is valid because every invocation
/// targets exactly one class, `sf`'s own restriction for that flag.
async fn run_apex_test(
    client: &ClientSocket,
    root: Option<&std::path::Path>,
    class_name: &str,
    method_name: Option<&str>,
) {
    let target = match method_name {
        Some(method) => format!("{class_name}.{method}"),
        None => class_name.to_string(),
    };
    let mut command = tokio::process::Command::new("sf");
    // `sf` resolves the SFDX project and its own default-org config
    // relative to its current directory -- must be the workspace root,
    // not whatever cwd apexls itself inherited at launch (see
    // `execute_command`'s doc comment).
    if let Some(root) = root {
        command.current_dir(root);
    }
    let output = command
        .args([
            "apex",
            "run",
            "test",
            "--tests",
            &target,
            "--result-format",
            "json",
            "--json",
            "--synchronous",
        ])
        .output()
        .await;
    let (message_type, message) = match output {
        Ok(output) => summarize_apex_test_output(&target, &output.stdout),
        Err(err) => (
            MessageType::ERROR,
            format!("apexls: couldn't run `sf apex run test --tests {target}`: {err}"),
        ),
    };
    let _ = client.notify::<lsp_types::notification::ShowMessage>(ShowMessageParams {
        typ: message_type,
        message,
    });
}

/// Parses `sf apex run test`'s combined `--result-format json --json`
/// stdout (see `run_apex_test`'s doc comment for the two shapes this
/// handles) into a `(MessageType, message)` pair -- `ERROR` for a
/// failing/uninterpretable/CLI-rejected run, `INFO` only when every test
/// actually passed.
fn summarize_apex_test_output(target: &str, stdout: &[u8]) -> (MessageType, String) {
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(stdout) else {
        return (
            MessageType::ERROR,
            format!("apexls: `sf apex run test --tests {target}` produced no parseable output"),
        );
    };
    if let Some(summary) = value.pointer("/result/summary") {
        let outcome = summary
            .get("outcome")
            .and_then(|v| v.as_str())
            .unwrap_or("Unknown");
        let passing = summary.get("passing").and_then(|v| v.as_i64()).unwrap_or(0);
        let failing = summary.get("failing").and_then(|v| v.as_i64()).unwrap_or(0);
        let tests_ran = summary
            .get("testsRan")
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        let mut message =
            format!("{target}: {outcome} ({passing} passing, {failing} failing, {tests_ran} ran)");
        if failing > 0 {
            if let Some(tests) = value.pointer("/result/tests").and_then(|v| v.as_array()) {
                for test in tests {
                    if test.get("Outcome").and_then(|v| v.as_str()) != Some("Fail") {
                        continue;
                    }
                    let name = test
                        .get("FullName")
                        .and_then(|v| v.as_str())
                        .unwrap_or(target);
                    let msg = test
                        .get("Message")
                        .and_then(|v| v.as_str())
                        .unwrap_or("(no message)");
                    message.push_str(&format!("\n  {name}: {msg}"));
                }
            }
        }
        let message_type = if outcome == "Passed" {
            MessageType::INFO
        } else {
            MessageType::ERROR
        };
        return (message_type, message);
    }
    let cli_message = value
        .get("message")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown error");
    (
        MessageType::ERROR,
        format!("apexls: `sf apex run test --tests {target}` failed: {cli_message}"),
    )
}

/// A single diagnostic in `apexls check`'s plain-text-report shape:
/// byte-offset-derived `line`/`col` (1-based, `col` counted in `char`s --
/// `apexls dead`'s own convention before it was folded into `check`)
/// rather than an LSP `Position`, so the CLI binary doesn't need
/// `lsp-types` as its own dependency just to print a report.
pub struct CliDiagnostic {
    pub line: usize,
    pub col: usize,
    pub severity: &'static str,
    pub message: String,
}

/// Every diagnostic in `file` -- syntax errors, dead code, unresolved
/// references, type mismatches, and everything else
/// `capabilities::diagnostics_for_file` combines for
/// `textDocument/publishDiagnostics` -- converted to [`CliDiagnostic`]'s
/// plain-text shape. The CLI analogue of `publish_diagnostics`'s per-file
/// call to the same underlying function.
pub fn diagnostics_for_file(program: &BoundProgram, file: apex_binder::FileId) -> Vec<CliDiagnostic> {
    let text = program.source_text(file);
    let index = line_index::LineIndex::new(&text);
    capabilities::diagnostics_for_file(program, file, PositionEncoding::Utf8)
        .into_iter()
        .map(|d| {
            let offset = index
                .to_offset(&text, d.range.start, PositionEncoding::Utf8)
                .unwrap_or(0);
            let (line, col) = index.line_col(&text, offset);
            let severity = match d.severity {
                Some(lsp_types::DiagnosticSeverity::WARNING) => "warning",
                Some(lsp_types::DiagnosticSeverity::INFORMATION) => "info",
                Some(lsp_types::DiagnosticSeverity::HINT) => "hint",
                _ => "error",
            };
            CliDiagnostic {
                line,
                col,
                severity,
                message: d.message,
            }
        })
        .collect()
}

/// One fix in `apexls fix`'s plain-text-report shape -- the same
/// byte-offset-derived `line`/`col` convention [`CliDiagnostic`] uses.
#[derive(Debug)]
pub struct CliFix {
    pub line: usize,
    pub col: usize,
    pub description: String,
}

/// The outcome of resolving one file's candidate fixes for `apexls fix`:
/// `new_text` is the whole rewritten file, ready to write to disk, `Some`
/// only when at least one fix actually applied. `conflicts` is every fix
/// skipped because of a genuine crossing overlap (see
/// `fix::resolve_fix_conflicts`'s own doc comment) -- worth reporting so a
/// human can look. A subsumed fix (dropped because another fix's range
/// fully contains it) appears in neither list: its target text is erased
/// by the containing fix regardless, so there's nothing to report.
pub struct CliFixResolution {
    pub applied: Vec<CliFix>,
    pub conflicts: Vec<CliFix>,
    pub new_text: Option<String>,
}

/// `apexls fix`'s own CLI-facing entry point: the same protocol-agnostic
/// candidate-fix production and conflict resolution
/// `capabilities::dead_code_actions` is built on, applied to the whole
/// file rather than filtered down to one LSP-requested range. The CLI
/// analogue of [`diagnostics_for_file`].
pub fn resolve_fixes_for_file(program: &BoundProgram, file: apex_binder::FileId) -> CliFixResolution {
    let text = program.source_text(file);
    let index = line_index::LineIndex::new(text);
    let to_cli = |f: &fix::CandidateFix| {
        let (line, col) = index.line_col(text, f.range.start().into());
        CliFix {
            line,
            col,
            description: f.description.clone(),
        }
    };

    let resolution = fix::resolve_fix_conflicts(fix::candidate_fixes_for_file(program, file));
    let applied: Vec<CliFix> = resolution.applied.iter().map(to_cli).collect();
    let conflicts: Vec<CliFix> = resolution.conflicts.iter().map(to_cli).collect();
    let new_text = if resolution.applied.is_empty() {
        None
    } else {
        Some(fix::apply_fixes(text, resolution.applied))
    };
    CliFixResolution {
        applied,
        conflicts,
        new_text,
    }
}

/// Runs the LSP server to completion over stdio -- the whole behavior of
/// both the `apexls-server` compatibility binary and `apexls server`.
/// Callers provide their own async runtime (both entry points use a
/// `#[tokio::main(flavor = "current_thread")]` `main`); this function
/// itself is runtime-agnostic beyond needing to run *on* one.
pub async fn run_server() {
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
            work_done_progress_supported: false,
            config: None,
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
