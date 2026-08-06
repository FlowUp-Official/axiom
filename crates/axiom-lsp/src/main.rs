use tower_lsp::{LspService, Server};

use axiom_lsp::server::AxiomServer;

#[tokio::main]
async fn main() {
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();

    let (service, socket) = LspService::new(AxiomServer::new);
    Server::new(stdin, stdout, socket).serve(service).await;
}
