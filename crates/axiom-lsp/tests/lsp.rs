//! End-to-end LSP integration tests.
//!
//! The server is driven over an in-memory `LspService` + `ClientSocket`
//! pair: requests/notifications are sent through the service and the
//! server-to-client messages (e.g. `textDocument/publishDiagnostics`) are
//! read off the socket stream.

use std::path::PathBuf;
use std::time::Duration;

use axiom_lsp::server::AxiomServer;
use futures::StreamExt;
use tower::ServiceExt;
use tower_lsp::jsonrpc::Request as RpcRequest;
use tower_lsp::lsp_types::*;
use tower_lsp::{ClientSocket, LspService};

fn rpc_request(method: &'static str, params: serde_json::Value) -> RpcRequest {
    RpcRequest::build(method).params(params).finish()
}

fn rpc_request_with_id(method: &'static str, params: serde_json::Value) -> RpcRequest {
    RpcRequest::build(method).id(1i64).params(params).finish()
}

/// Send a request and return the parsed result value.
async fn call(
    service: &mut LspService<AxiomServer>,
    method: &'static str,
    params: serde_json::Value,
) -> serde_json::Value {
    let response = service
        .oneshot(rpc_request_with_id(method, params))
        .await
        .unwrap()
        .expect("request must produce a response");
    response
        .into_parts()
        .1
        .unwrap_or_else(|e| panic!("request failed: {e}"))
}

/// Send a notification (fire and forget).
async fn notify(
    service: &mut LspService<AxiomServer>,
    method: &'static str,
    params: serde_json::Value,
) {
    service
        .oneshot(rpc_request(method, params))
        .await
        .unwrap();
}

/// A small, valid Axiom workspace on disk.
fn workspace() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().to_path_buf();

    std::fs::write(
        base.join("axiom.json"),
        r#"{
            "$schema": "https://raw.githubusercontent.com/FlowUp-Official/axiom/v0.6.0/schemas/axiom.schema.json",
            "project": { "name": "fixture", "dialect": "postgres" },
            "cache": { "enabled": false, "path": ".axiom.cache" },
            "inputs": {
                "schema": ["schema.sql"],
                "queries": ["queries/**/*.sql"],
                "models": ["models/**/*.axm"]
            },
            "validation": { "on_error": "fail" },
            "outputs": {
                "api": { "type": "typescript", "path": "gen/api.ts" }
            }
        }"#,
    )
    .unwrap();

    std::fs::write(
        base.join("schema.sql"),
        "CREATE TABLE users (\n  id BIGSERIAL PRIMARY KEY,\n  email VARCHAR(255) NOT NULL\n);\n",
    )
    .unwrap();

    std::fs::create_dir(base.join("queries")).unwrap();
    std::fs::write(base.join("queries/one.sql"), "SELECT * FROM users;\n").unwrap();

    std::fs::create_dir(base.join("models")).unwrap();
    std::fs::write(
        base.join("models/user.axm"),
        "export model User {\n  email: string\n}\n",
    )
    .unwrap();

    (dir, base)
}

async fn setup(    base: &std::path::Path,
) -> (LspService<AxiomServer>, ClientSocket, Url) {
    let (service, socket) = LspService::new(AxiomServer::new);
    let mut service = service;

    let root_uri = Url::from_directory_path(base).unwrap();
    call(
        &mut service,
        "initialize",
        serde_json::json!({ "capabilities": {}, "rootUri": root_uri }),
    )
    .await;
    notify(&mut service, "initialized", serde_json::json!({})).await;

    (service, socket, root_uri)
}

fn file_uri(base: &std::path::Path, name: &str) -> Url {
    Url::from_file_path(base.join(name)).unwrap()
}

/// Collects server-to-client messages from the socket until a
/// `textDocument/publishDiagnostics` for a URI matching `matches` arrives.
async fn wait_for_publish<F>(
    socket: &mut ClientSocket,
    matches: F,
    timeout: Duration,
) -> (Url, Vec<Diagnostic>)
where
    F: Fn(&Url) -> bool,
{
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_millis(50), socket.next()).await {
            Ok(Some(req)) if req.method() == "textDocument/publishDiagnostics" => {
                let (uri, diags) = publish_diags(req.params().cloned().unwrap_or_default());
                if matches(&uri) {
                    return (uri, diags);
                }
            }
            _ => continue,
        }
    }
    panic!("timed out waiting for publishDiagnostics");
}

fn publish_diags(params: serde_json::Value) -> (Url, Vec<Diagnostic>) {
    let p: PublishDiagnosticsParams = serde_json::from_value(params).unwrap();
    (p.uri, p.diagnostics)
}

// ---------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------

#[tokio::test]
async fn initialize_advertises_supported_capabilities() {
    let (dir, base) = workspace();
    let (service, _socket) = LspService::new(AxiomServer::new);
    let mut service = service;

    let value = call(
        &mut service,
        "initialize",
        serde_json::json!({ "capabilities": {}, "rootUri": Url::from_directory_path(&base).unwrap() }),
    )
    .await;
    let result: InitializeResult = serde_json::from_value(value).unwrap();
    let info = result.server_info.unwrap();
    assert_eq!(info.name, "axiom-lsp");
    assert!(result.capabilities.hover_provider.is_some());
    assert!(result.capabilities.definition_provider.is_some());
    assert!(result.capabilities.rename_provider.is_some());
    assert!(result.capabilities.document_formatting_provider.is_some());
    drop((dir, service));
}

#[tokio::test]
async fn workspace_load_publishes_diagnostics() {
    let (dir, base) = workspace();
    let (service, mut socket, _root) = setup(&base).await;

    // `queries/one.sql` references `users`, which exists -> no diagnostics.
    let (uri, diags) = wait_for_publish(
        &mut socket,
        |u| u.path().ends_with("one.sql"),
        Duration::from_secs(2),
    )
    .await;
    assert_eq!(diags.len(), 0, "expected no diagnostics, got {diags:?}");
    assert!(uri.path().ends_with("one.sql"));
    drop((dir, service));
}

#[tokio::test]
async fn did_open_reports_parse_errors_for_unknown_files() {
    let (dir, base) = workspace();
    let (mut service, mut socket, _root) = setup(&base).await;

    let broken = file_uri(&base, "scratch.sql");
    notify(
        &mut service,
        "textDocument/didOpen",
        serde_json::json!({
            "textDocument": {
                "uri": broken,
                "languageId": "sql",
                "version": 1,
                "text": "CREATE TABLE broken (id SERIAL,);"
            }
        }),
    )
    .await;

    let (uri, diags) = wait_for_publish(
        &mut socket,
        |u| u == &broken,
        Duration::from_secs(2),
    )
    .await;
    assert_eq!(uri, broken);
    assert!(!diags.is_empty(), "expected at least one diagnostic");
    assert!(
        diags.iter().any(|d| d.severity == Some(DiagnosticSeverity::ERROR)),
        "expected an error severity, got {diags:?}"
    );
    drop((dir, service));
}

#[tokio::test]
async fn completion_suggests_columns_after_qualifier() {
    let (dir, base) = workspace();
    let (mut service, _socket, _root) = setup(&base).await;

    // `SELECT u.| FROM users u` — offset just after the dot.
    let uri = file_uri(&base, "queries/one.sql");
    notify(
        &mut service,
        "textDocument/didOpen",
        serde_json::json!({
            "textDocument": {
                "uri": uri,
                "languageId": "sql",
                "version": 1,
                "text": "SELECT u. FROM users u;"
            }
        }),
    )
    .await;

    let value = call(
        &mut service,
        "textDocument/completion",
        serde_json::json!({
            "textDocument": { "uri": uri },
            "position": { "line": 0, "character": 9 }
        }),
    )
    .await;
    let response: Option<CompletionResponse> = serde_json::from_value(value).unwrap();
    let Some(CompletionResponse::Array(items)) = response else {
        panic!("expected completion array");
    };
    let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
    assert!(labels.contains(&"id"), "got {labels:?}");
    assert!(labels.contains(&"email"), "got {labels:?}");
    drop((dir, service));
}

#[tokio::test]
async fn hover_describes_table_and_columns() {
    let (dir, base) = workspace();
    let (mut service, _socket, _root) = setup(&base).await;

    let uri = file_uri(&base, "schema.sql");
    notify(
        &mut service,
        "textDocument/didOpen",
        serde_json::json!({
            "textDocument": {
                "uri": uri,
                "languageId": "sql",
                "version": 1,
                "text": "CREATE TABLE users (\n  id BIGSERIAL PRIMARY KEY,\n  email VARCHAR(255) NOT NULL\n);\n"
            }
        }),
    )
    .await;

    let value = call(
        &mut service,
        "textDocument/hover",
        serde_json::json!({
            "textDocument": { "uri": uri },
            "position": { "line": 0, "character": 13 }
        }),
    )
    .await;
    let response: Option<Hover> = serde_json::from_value(value).unwrap();
    let hover = response.expect("hover expected over table name");
    assert!(hover.range.is_some());
    drop((dir, service));
}

#[tokio::test]
async fn goto_definition_jumps_to_schema_declaration() {
    let (dir, base) = workspace();
    let (mut service, _socket, _root) = setup(&base).await;

    let uri = file_uri(&base, "queries/one.sql");
    notify(
        &mut service,
        "textDocument/didOpen",
        serde_json::json!({
            "textDocument": {
                "uri": uri,
                "languageId": "sql",
                "version": 1,
                "text": "SELECT * FROM users;"
            }
        }),
    )
    .await;

    // Cursor on `users` (byte 14).
    let value = call(
        &mut service,
        "textDocument/definition",
        serde_json::json!({
            "textDocument": { "uri": uri },
            "position": { "line": 0, "character": 14 }
        }),
    )
    .await;
    let response: Option<GotoDefinitionResponse> = serde_json::from_value(value).unwrap();
    let Some(GotoDefinitionResponse::Link(links)) = response else {
        panic!("expected a definition link");
    };
    assert!(
        links[0].target_uri.path().ends_with("schema.sql"),
        "got {:?}",
        links[0].target_uri
    );
    drop((dir, service));
}

#[tokio::test]
async fn rename_updates_uses_across_files() {
    let (dir, base) = workspace();
    let (mut service, _socket, _root) = setup(&base).await;

    let schema_uri = file_uri(&base, "schema.sql");
    notify(
        &mut service,
        "textDocument/didOpen",
        serde_json::json!({
            "textDocument": {
                "uri": schema_uri,
                "languageId": "sql",
                "version": 1,
                "text": "CREATE TABLE users (\n  id BIGSERIAL PRIMARY KEY,\n  email VARCHAR(255) NOT NULL\n);\n"
            }
        }),
    )
    .await;
    let query_uri = file_uri(&base, "queries/one.sql");
    notify(
        &mut service,
        "textDocument/didOpen",
        serde_json::json!({
            "textDocument": {
                "uri": query_uri,
                "languageId": "sql",
                "version": 1,
                "text": "SELECT * FROM users;"
            }
        }),
    )
    .await;

    // Rename `users` (declared at line 0, char 13) to `people`.
    let value = call(
        &mut service,
        "textDocument/rename",
        serde_json::json!({
            "textDocument": { "uri": schema_uri },
            "position": { "line": 0, "character": 13 },
            "newName": "people"
        }),
    )
    .await;
    let Some(edit) = serde_json::from_value::<Option<WorkspaceEdit>>(value).unwrap() else {
        panic!("expected a workspace edit");
    };
    let changes = edit.changes.unwrap();
    assert!(changes.contains_key(&schema_uri));
    assert!(changes.contains_key(&query_uri));
    drop((dir, service));
}

#[tokio::test]
async fn formatting_returns_full_document_edit() {
    let (dir, base) = workspace();
    let (mut service, _socket, _root) = setup(&base).await;

    let uri = file_uri(&base, "queries/one.sql");
    notify(
        &mut service,
        "textDocument/didOpen",
        serde_json::json!({
            "textDocument": {
                "uri": uri,
                "languageId": "sql",
                "version": 1,
                "text": "select   *   from   users;"
            }
        }),
    )
    .await;

    let value = call(
        &mut service,
        "textDocument/formatting",
        serde_json::json!({
            "textDocument": { "uri": uri },
            "options": { "tabSize": 4, "insertSpaces": true }
        }),
    )
    .await;
    let edits: Vec<TextEdit> = serde_json::from_value(value).unwrap();
    assert!(!edits.is_empty(), "expected a formatting edit");
    drop((dir, service));
}

#[tokio::test]
async fn broken_config_publishes_error_diagnostic() {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().to_path_buf();
    std::fs::write(base.join("axiom.json"), "this is not json").unwrap();

    let (service, mut socket, _root) = setup(&base).await;

    let config_uri = file_uri(&base, "axiom.json");
    let (uri, diags) = wait_for_publish(
        &mut socket,
        |u| u == &config_uri,
        Duration::from_secs(2),
    )
    .await;
    assert_eq!(uri, config_uri);
    assert_eq!(diags.len(), 1, "expected one config error, got {diags:?}");
    assert_eq!(diags[0].severity, Some(DiagnosticSeverity::ERROR));
    assert!(
        diags[0].message.contains("Failed to parse configuration"),
        "unexpected message: {}",
        diags[0].message
    );
    drop((dir, service));
}

#[tokio::test]
async fn workspace_load_failure_publishes_error_diagnostic() {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().to_path_buf();
    // Valid JSON, but an invalid glob pattern makes the workspace load fail.
    std::fs::write(
        base.join("axiom.json"),
        r#"{
            "project": { "name": "fixture", "dialect": "postgres" },
            "cache": { "enabled": false },
            "validation": { "on_error": "fail" },
            "inputs": { "schema": ["["], "queries": [], "models": [] },
            "outputs": {}
        }"#,
    )
    .unwrap();

    let (service, mut socket, _root) = setup(&base).await;

    let config_uri = file_uri(&base, "axiom.json");
    let (uri, diags) = wait_for_publish(
        &mut socket,
        |u| u == &config_uri,
        Duration::from_secs(2),
    )
    .await;
    assert_eq!(uri, config_uri);
    assert_eq!(diags.len(), 1, "expected one load error, got {diags:?}");
    assert_eq!(diags[0].severity, Some(DiagnosticSeverity::ERROR));
    assert!(
        diags[0].message.contains("Failed to load workspace"),
        "unexpected message: {}",
        diags[0].message
    );
    drop((dir, service));
}
