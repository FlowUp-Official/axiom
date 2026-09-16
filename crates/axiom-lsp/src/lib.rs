//! Axiom Language Server: LSP transport and handlers over the incremental
//! analysis engine.

pub mod handlers;
pub mod server;

use tower_lsp::{LspService, Server};

use crate::server::AxiomServer;

/// Serve the language server over stdin/stdout until the client disconnects.
///
/// Shared by the standalone `axiom-lsp` binary and the `axiom lsp` CLI
/// subcommand so the LSP runs without a separate `axiom-lsp` install.
pub async fn run_stdio_server() {
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();

    let (service, socket) = LspService::new(AxiomServer::new);
    Server::new(stdin, stdout, socket).serve(service).await;
}
