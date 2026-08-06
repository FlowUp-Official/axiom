//! The Axiom language server: transport glue between LSP and the incremental
//! analysis engine in `axiom-analysis`.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use axiom_analysis::{AnalysisDatabase, ChangeKind, Role};
use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::*;
use tower_lsp::{Client, LanguageServer};

use crate::handlers;

/// The shared server state: one incremental database per workspace.
struct State {
    db: AnalysisDatabase,
    base: PathBuf,
    loaded: bool,
}

impl Default for State {
    fn default() -> Self {
        Self {
            db: AnalysisDatabase::new(),
            base: PathBuf::new(),
            loaded: false,
        }
    }
}

pub struct AxiomServer {
    client: Client,
    state: Mutex<State>,
}

impl AxiomServer {
    pub fn new(client: Client) -> Self {
        Self {
            client,
            state: Mutex::new(State::default()),
        }
    }

    fn publish_workspace(&self) {
        let pairs: Vec<(Url, Vec<Diagnostic>)> = {
            let mut state = self.state.lock().unwrap();
            let paths: Vec<PathBuf> = state
                .db
                .file_paths()
                .map(Path::to_path_buf)
                .collect();
            paths
                .iter()
                .flat_map(|p| handlers::diagnostics::lsp_diagnostics(&mut state.db, p))
                .collect()
        };
        for (uri, diags) in pairs {
            let client = self.client.clone();
            tokio::spawn(async move {
                client.publish_diagnostics(uri, diags, None).await;
            });
        }
    }

    fn publish_after_change(&self, changed: &Path, change: &ChangeKind) {
        let pairs: Vec<(Url, Vec<Diagnostic>)> = {
            let mut state = self.state.lock().unwrap();
            let affected: Vec<PathBuf> = state.db.affected_files(changed, change);
            affected
                .iter()
                .flat_map(|p| handlers::diagnostics::lsp_diagnostics(&mut state.db, p))
                .collect()
        };
        for (uri, diags) in pairs {
            let client = self.client.clone();
            tokio::spawn(async move {
                client.publish_diagnostics(uri, diags, None).await;
            });
        }
    }

    /// Publish a single error diagnostic on the given file, used to surface
    /// configuration and workspace-load failures that have no source file of
    /// their own.
    fn publish_error(&self, path: &Path, message: String) {
        let Some(uri) = Url::from_file_path(path).ok() else {
            return;
        };
        let client = self.client.clone();
        tokio::spawn(async move {
            client
                .publish_diagnostics(
                    uri,
                    vec![Diagnostic {
                        range: Range::new(Position::new(0, 0), Position::new(0, 0)),
                        severity: Some(DiagnosticSeverity::ERROR),
                        message,
                        source: Some("axiom-lsp".to_string()),
                        ..Diagnostic::default()
                    }],
                    None,
                )
                .await;
        });
    }
}

#[tower_lsp::async_trait]
impl LanguageServer for AxiomServer {
    async fn initialize(&self, params: InitializeParams) -> Result<InitializeResult> {
        let base = params
            .workspace_folders
            .and_then(|folders| folders.first().map(|f| f.uri.clone()))
            .or(params.root_uri)
            .and_then(|uri| uri.to_file_path().ok())
            .unwrap_or_else(std::env::temp_dir);
        self.state.lock().unwrap().base = base;

        let capabilities = ServerCapabilities {
            text_document_sync: Some(TextDocumentSyncCapability::Kind(
                TextDocumentSyncKind::FULL,
            )),
            hover_provider: Some(HoverProviderCapability::Simple(true)),
            completion_provider: Some(CompletionOptions {
                trigger_characters: Some(vec![".".to_string(), ":".to_string()]),
                ..CompletionOptions::default()
            }),
            definition_provider: Some(OneOf::Left(true)),
            rename_provider: Some(OneOf::Left(true)),
            document_formatting_provider: Some(OneOf::Left(true)),
            ..ServerCapabilities::default()
        };

        Ok(InitializeResult {
            capabilities,
            server_info: Some(ServerInfo {
                name: "axiom-lsp".to_string(),
                version: Some(env!("CARGO_PKG_VERSION").to_string()),
            }),
        })
    }

    async fn initialized(&self, _: InitializedParams) {
        let base = self.state.lock().unwrap().base.clone();
        let config_path = base.join("axiom.json");

        // Discover and load the project config, if any. Without one the server
        // still serves single-file diagnostics for open buffers.
        let (config, found_at) = match axiom_core::config::AxiomConfig::find_and_load(
            Some(&config_path),
        ) {
            Ok(found) => found,
            Err(err) => {
                // A missing config is a normal single-file setup; an existing
                // but broken one is a problem the user should see.
                if config_path.exists() {
                    self.publish_error(&config_path, err.to_string());
                }
                self.state.lock().unwrap().loaded = true;
                return;
            }
        };

        let cache_path = if config.cache.enabled {
            Some(base.join(&config.cache.path))
        } else {
            None
        };
        let tools = cache_path
            .as_ref()
            .map(|p| axiom_core::cache::ToolCache::open(p));

        let load_result = {
            let mut state = self.state.lock().unwrap();
            state.db.set_cache(tools, cache_path);
            let result = state.db.load_workspace(&config, &base);
            state.loaded = true;
            result
        };
        if let Err(err) = load_result {
            self.publish_error(
                &found_at,
                format!("Failed to load workspace from {}: {err}", found_at.display()),
            );
        }

        self.publish_workspace();
    }

    async fn shutdown(&self) -> Result<()> {
        Ok(())
    }

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        let uri = params.text_document.uri;
        let Some(path) = uri.to_file_path().ok() else {
            return;
        };
        let change = {
            let mut state = self.state.lock().unwrap();
            state.db.open(&path, params.text_document.text)
        };
        self.publish_after_change(&path, &change);
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        let uri = params.text_document.uri;
        let Some(path) = uri.to_file_path().ok() else {
            return;
        };
        let Some(text) = params.content_changes.last().map(|c| c.text.clone()) else {
            return;
        };
        let change = {
            let mut state = self.state.lock().unwrap();
            state.db.open(&path, text)
        };
        if !change.is_none() {
            self.publish_after_change(&path, &change);
        }
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        let uri = params.text_document.uri;
        let Some(path) = uri.to_file_path().ok() else {
            return;
        };
        {
            let mut state = self.state.lock().unwrap();
            if state.db.file_role(&path) == Role::Unknown {
                state.db.close(&path);
            }
        }
        self.client
            .publish_diagnostics(uri, Vec::new(), None)
            .await;
    }

    async fn completion(&self, params: CompletionParams) -> Result<Option<CompletionResponse>> {
        let uri = params.text_document_position.text_document.uri;
        let Some(path) = uri.to_file_path().ok() else {
            return Ok(None);
        };
        let position = params.text_document_position.position;
        let items = {
            let mut state = self.state.lock().unwrap();
            handlers::completion::lsp_completion(&mut state.db, &path, position)
        };
        if items.is_empty() {
            return Ok(None);
        }
        Ok(Some(CompletionResponse::Array(items)))
    }

    async fn hover(&self, params: HoverParams) -> Result<Option<Hover>> {
        let uri = params.text_document_position_params.text_document.uri;
        let Some(path) = uri.to_file_path().ok() else {
            return Ok(None);
        };
        let position = params.text_document_position_params.position;
        let hover = {
            let mut state = self.state.lock().unwrap();
            handlers::hover::lsp_hover(&mut state.db, &path, position)
        };
        Ok(hover)
    }

    async fn goto_definition(
        &self,
        params: GotoDefinitionParams,
    ) -> Result<Option<GotoDefinitionResponse>> {
        let uri = params.text_document_position_params.text_document.uri;
        let Some(path) = uri.to_file_path().ok() else {
            return Ok(None);
        };
        let position = params.text_document_position_params.position;
        let definition = {
            let mut state = self.state.lock().unwrap();
            handlers::definition::lsp_definition(&mut state.db, &path, position)
        };
        Ok(definition)
    }

    async fn rename(&self, params: RenameParams) -> Result<Option<WorkspaceEdit>> {
        let uri = params.text_document_position.text_document.uri;
        let Some(path) = uri.to_file_path().ok() else {
            return Ok(None);
        };
        let position = params.text_document_position.position;
        let edit = {
            let mut state = self.state.lock().unwrap();
            handlers::rename::lsp_rename(&mut state.db, &path, position, &params.new_name)
        };
        Ok(edit)
    }

    async fn formatting(
        &self,
        params: DocumentFormattingParams,
    ) -> Result<Option<Vec<TextEdit>>> {
        let uri = params.text_document.uri;
        let Some(path) = uri.to_file_path().ok() else {
            return Ok(None);
        };
        let edits = {
            let state = self.state.lock().unwrap();
            let text = state.db.file_text(&path).unwrap_or("");
            handlers::formatting::lsp_formatting(&path, text)
        };
        Ok(Some(edits))
    }
}
