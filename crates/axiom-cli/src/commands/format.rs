//! `axiom format` — canonical formatting for `.axm` and SQL inputs.

use std::path::{Path, PathBuf};

use axiom_core::config::{resolve_glob_paths, AxiomConfig};
use axiom_core::errors::AxiomError;
use axiom_diagnostics::{render_all, Diagnostic, Renderer, summary};
use owo_colors::{OwoColorize, Stream};

use crate::commands::base_dir;
use crate::commands::tty::is_tty;
use crate::FormatArgs;

/// Format every configured input (or the explicit `--files`), writing the
/// canonical form back to disk. With `--check` nothing is written; the run
/// exits non-zero when any file would change.
pub fn run(args: FormatArgs, config: &AxiomConfig, config_path: &Path) -> Result<i32, AxiomError> {
    let base = base_dir(config_path);
    let targets = resolve_targets(config, &base, &args.files)?;
    let renderer = Renderer::new(is_tty());

    let mut diagnostics = Vec::new();
    let mut changed = 0usize;

    for path in &targets {
        let src = std::fs::read_to_string(path)?;
        match axiom_format::format_source(&path.to_string_lossy(), &src) {
            None => continue,
            Some(Err(message)) => {
                diagnostics.push(
                    Diagnostic::error(
                        path,
                        "format.parse",
                        format!("failed to format: {message}"),
                    )
                    .with_help("fix the syntax error so the file can be parsed"),
                );
                continue;
            }
            Some(Ok(canonical)) => {
                if canonical == src {
                    continue;
                }
                changed += 1;
                if args.check {
                    diagnostics.push(
                        Diagnostic::warning(
                            path,
                            "format.would-reformat",
                            "file is not formatted",
                        )
                        .with_help("run `axiom format` to reformat it"),
                    );
                    continue;
                }
                std::fs::write(path, canonical)?;
            }
        }
    }

    if !diagnostics.is_empty() {
        eprint!("{}", render_all(&renderer, &diagnostics, |_| None));
    }

    let (errors, _) = summary(&diagnostics);

    if args.check {
        if changed > 0 {
            eprintln!(
                "{} {}",
                format!("{changed} file(s) would be reformatted")
                    .if_supports_color(Stream::Stderr, |s| s.yellow().bold().to_string()),
                "(run `axiom format` to apply)"
                    .if_supports_color(Stream::Stderr, |s| s.dimmed().to_string()),
            );
            return Ok(2);
        }
        if errors > 0 {
            return Ok(1);
        }
        println!(
            "{}",
            "All files are formatted".if_supports_color(Stream::Stdout, |s| {
                s.green().bold().to_string()
            }),
        );
        return Ok(0);
    }

    if errors > 0 {
        return Ok(1);
    }
    let verb = if changed == 1 { "file" } else { "files" };
    println!(
        "{} {}",
        format!("formatted {changed} {verb}")
            .if_supports_color(Stream::Stdout, |s| s.green().bold().to_string()),
        if changed == 0 {
            "(already canonical)".to_string()
        } else {
            String::new()
        }
        .if_supports_color(Stream::Stdout, |s| s.dimmed().to_string()),
    );
    Ok(0)
}

/// Resolve the files to format: explicit `--files` (globs allowed) when given,
/// otherwise the configured schema and model inputs.
fn resolve_targets(
    config: &AxiomConfig,
    base: &Path,
    explicit: &[PathBuf],
) -> Result<Vec<PathBuf>, AxiomError> {
    if !explicit.is_empty() {
        let patterns: Vec<String> = explicit
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect();
        return resolve_glob_paths(&patterns, base);
    }

    let mut patterns: Vec<String> = Vec::new();
    patterns.extend(config.inputs.schema.iter().cloned());
    patterns.extend(config.inputs.models.iter().cloned());
    let mut files = Vec::new();
    for path in resolve_glob_paths(&patterns, base)? {
        if path.extension().is_some_and(|e| e == "sql" || e == "axm") {
            files.push(path);
        }
    }
    Ok(files)
}
