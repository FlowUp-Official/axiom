//! Zed extension for Axiom. Resolves and launches the `axiom-lsp` binary.
//!
//! The language server is *not* shipped inside the extension: Zed extensions
//! must not bundle language servers. The binary is looked up on the worktree
//! `$PATH`, or the `AXIOM_LSP_BIN` environment variable may point at a
//! specific build (e.g. `target/debug/axiom-lsp`).

use zed_extension_api::{self as zed, LanguageServerId, Result, Worktree};

struct AxiomExtension;

impl zed::Extension for AxiomExtension {
    fn new() -> Self {
        Self
    }

    fn language_server_command(
        &mut self,
        _language_server_id: &LanguageServerId,
        worktree: &Worktree,
    ) -> Result<zed::Command> {
        let binary = std::env::var("AXIOM_LSP_BIN")
            .ok()
            .filter(|p| !p.is_empty())
            .or_else(|| worktree.which("axiom-lsp"))
            .ok_or_else(|| {
                "axiom-lsp not found on PATH. Install it with `cargo install --path crates/axiom-lsp` \
                 or set the AXIOM_LSP_BIN environment variable to the binary path."
                    .to_string()
            })?;

        Ok(zed::Command {
            command: binary,
            args: Vec::new(),
            env: worktree.shell_env(),
        })
    }
}

zed::register_extension!(AxiomExtension);
