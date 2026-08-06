//! `.axm` import resolution and cross-file linking.
//!
//! Model files may import models from sibling files. The resolver:
//!
//! * parses every file,
//! * detects duplicate model names,
//! * resolves `import { X } from "path"` statements to concrete files,
//! * verifies every referenced type is in scope,
//! * detects import cycles with a diagnostic chain.
//!
//! The resulting [`ModelRegistry`] is a flattened, deterministic view of all
//! models that the code generators emit.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::axm::ast::{AxmFile, TypeRef};
use crate::axm::parser::parse_axm_file;
use crate::errors::AxiomError;

/// A model together with the file it was declared in.
#[derive(Debug, Clone)]
pub struct ResolvedModel {
    pub path: PathBuf,
    pub model: crate::axm::ast::ModelDecl,
}

/// The linked set of all models across a group of `.axm` files.
#[derive(Debug, Default)]
pub struct ModelRegistry {
    /// Models in file/declaration order.
    pub models: Vec<ResolvedModel>,
    /// Model name -> index into [`ModelRegistry::models`].
    pub index: BTreeMap<String, usize>,
}

impl ModelRegistry {
    pub fn is_empty(&self) -> bool {
        self.models.is_empty()
    }

    pub fn model_by_name(&self, name: &str) -> Option<&ResolvedModel> {
        self.index.get(name).map(|&i| &self.models[i])
    }
}

/// Parse and link every source file into a [`ModelRegistry`].
pub fn resolve_models(sources: &[(PathBuf, String)]) -> Result<ModelRegistry, AxiomError> {
    let mut files: Vec<(PathBuf, AxmFile)> = Vec::with_capacity(sources.len());
    for (path, src) in sources {
        let file = parse_axm_file(src).map_err(|e| AxiomError::ModelParseError {
            path: path.clone(),
            message: e.to_string(),
        })?;
        files.push((path.clone(), file));
    }

    // Duplicate model names across files are an error: the flattened registry
    // shares one namespace.
    let mut name_to_file: BTreeMap<String, usize> = BTreeMap::new();
    for (file_idx, (path, file)) in files.iter().enumerate() {
        for model in &file.models {
            if let Some(prev) = name_to_file.get(&model.name) {
                return Err(AxiomError::ModelDuplicate {
                    name: model.name.clone(),
                    first: files[*prev].0.display().to_string(),
                    second: path.display().to_string(),
                });
            }
            name_to_file.insert(model.name.clone(), file_idx);
        }
    }

    // Resolve imports and record which model names each file may reference.
    let mut allowed: BTreeMap<usize, BTreeSet<String>> = BTreeMap::new();
    let mut edges: Vec<(usize, usize)> = Vec::new();
    for (file_idx, (path, file)) in files.iter().enumerate() {
        let mut names: BTreeSet<String> = file.models.iter().map(|m| m.name.clone()).collect();
        for import in &file.imports {
            let target = resolve_import_path(path, &import.source);
            let Some(target_idx) = files
                .iter()
                .position(|(p, _)| canonical(p) == canonical(&target))
            else {
                return Err(AxiomError::ModelResolutionError {
                    path: path.clone(),
                    message: format!(
                        "cannot resolve import `{}` from `{}`",
                        import.source,
                        path.display()
                    ),
                });
            };
            edges.push((file_idx, target_idx));
            for name in &import.names {
                if files[target_idx].1.model_by_name(name).is_none() {
                    return Err(AxiomError::ModelResolutionError {
                        path: path.clone(),
                        message: format!(
                            "imported model `{name}` does not exist in `{}`",
                            import.source
                        ),
                    });
                }
                names.insert(name.clone());
            }
        }
        allowed.insert(file_idx, names);
    }

    detect_cycles(&files, &edges)?;

    // Every named field type must be defined locally or imported.
    for (file_idx, (path, file)) in files.iter().enumerate() {
        let scope = &allowed[&file_idx];
        for model in &file.models {
            for field in &model.fields {
                check_type_refs(&field.ty, scope, &model.name, &field.name, path)?;
            }
        }
    }

    let mut registry = ModelRegistry::default();
    for (path, file) in files.iter() {
        for model in &file.models {
            let pos = registry.models.len();
            registry.index.insert(model.name.clone(), pos);
            registry.models.push(ResolvedModel {
                path: path.clone(),
                model: model.clone(),
            });
        }
    }

    Ok(registry)
}

/// Resolve `import { ... } from "path"` to a concrete `.axm` file, relative to
/// the importing file. A missing extension defaults to `.axm`.
fn resolve_import_path(current_file: &Path, source: &str) -> PathBuf {
    let base = current_file.parent().unwrap_or_else(|| Path::new("."));
    let mut path = base.join(source);
    if path.extension().is_none() {
        path.set_extension("axm");
    }
    path
}

fn canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

fn check_type_refs(
    ty: &TypeRef,
    scope: &BTreeSet<String>,
    model: &str,
    field: &str,
    path: &Path,
) -> Result<(), AxiomError> {
    match ty {
        TypeRef::Array(inner) => check_type_refs(inner, scope, model, field, path),
        TypeRef::Named(name) => {
            if scope.contains(name) {
                Ok(())
            } else {
                Err(AxiomError::ModelResolutionError {
                    path: path.to_path_buf(),
                    message: format!(
                        "unknown type `{name}` in field `{field}` of model `{model}` \
                         (not defined in this file and not imported)"
                    ),
                })
            }
        }
        _ => Ok(()),
    }
}

const WHITE: u8 = 0;
const GRAY: u8 = 1;
const BLACK: u8 = 2;

/// Detect import cycles with an iterative DFS and report the offending chain.
fn detect_cycles(
    files: &[(PathBuf, AxmFile)],
    edges: &[(usize, usize)],
) -> Result<(), AxiomError> {
    let n = files.len();
    let mut adjacency: Vec<Vec<usize>> = vec![Vec::new(); n];
    for &(from, to) in edges {
        if !adjacency[from].contains(&to) {
            adjacency[from].push(to);
        }
    }

    let mut color = vec![WHITE; n];
    let mut stack: Vec<usize> = Vec::new();

    for start in 0..n {
        if color[start] != WHITE {
            continue;
        }
        if let Some(cycle) = dfs(start, &adjacency, &mut color, &mut stack) {
            let chain: Vec<String> = cycle
                .iter()
                .map(|&i| {
                    files[i]
                        .0
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_else(|| files[i].0.display().to_string())
                })
                .collect();
            return Err(AxiomError::ModelImportCycle {
                chain: chain.join(" -> "),
            });
        }
    }

    Ok(())
}

fn dfs(
    u: usize,
    adjacency: &[Vec<usize>],
    color: &mut [u8],
    stack: &mut Vec<usize>,
) -> Option<Vec<usize>> {
    color[u] = GRAY;
    stack.push(u);
    for &v in &adjacency[u] {
        if color[v] == GRAY {
            let start = stack.iter().position(|&x| x == v).expect("v is on the stack");
            let mut cycle = stack[start..].to_vec();
            cycle.push(v);
            return Some(cycle);
        }
        if color[v] == WHITE && let Some(cycle) = dfs(v, adjacency, color, stack) {
            return Some(cycle);
        }
    }
    stack.pop();
    color[u] = BLACK;
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(path: &str, src: &str) -> (PathBuf, String) {
        (PathBuf::from(path), src.to_string())
    }

    #[test]
    fn links_imported_models_across_files() {
        let registry = resolve_models(&[
            file(
                "models/address.axm",
                "export model Address { street: string }",
            ),
            file(
                "models/user.axm",
                "import { Address } from \"address\"\nexport model User { billing: Address }",
            ),
        ])
        .expect("resolve");
        assert_eq!(registry.models.len(), 2);
        assert!(registry.model_by_name("Address").is_some());
        assert!(registry.model_by_name("User").is_some());
    }

    #[test]
    fn detects_unresolvable_import() {
        let err = resolve_models(&[file(
            "models/user.axm",
            "import { Address } from \"missing\"\nexport model User { billing: Address }",
        )])
        .expect_err("missing file");
        assert!(
            matches!(&err, AxiomError::ModelResolutionError { message, .. }
                if message.contains("cannot resolve import")),
            "{err}"
        );
    }

    #[test]
    fn detects_import_of_nonexistent_model() {
        let err = resolve_models(&[
            file("models/address.axm", "export model Address { street: string }"),
            file(
                "models/user.axm",
                "import { Nope } from \"address\"\nexport model User { }",
            ),
        ])
        .expect_err("missing model");
        assert!(
            matches!(&err, AxiomError::ModelResolutionError { message, .. }
                if message.contains("does not exist")),
            "{err}"
        );
    }

    #[test]
    fn detects_duplicate_model_names() {
        let err = resolve_models(&[
            file("models/a.axm", "export model User { a: string }"),
            file("models/b.axm", "export model User { b: string }"),
        ])
        .expect_err("duplicate");
        assert!(matches!(&err, AxiomError::ModelDuplicate { name, .. } if name == "User"), "{err}");
    }

    #[test]
    fn detects_unreferenced_type() {
        let err = resolve_models(&[file(
            "models/user.axm",
            "export model User { billing: Address }",
        )])
        .expect_err("unknown type");
        assert!(
            matches!(&err, AxiomError::ModelResolutionError { message, .. }
                if message.contains("unknown type `Address`")),
            "{err}"
        );
    }

    #[test]
    fn detects_import_cycle_direct() {
        let err = resolve_models(&[
            file("models/a.axm", "import { B } from \"b\"\nexport model A { b: B }"),
            file("models/b.axm", "import { A } from \"a\"\nexport model B { a: A }"),
        ])
        .expect_err("cycle");
        assert!(
            matches!(&err, AxiomError::ModelImportCycle { chain } if chain.contains("a.axm") && chain.contains("b.axm")),
            "{err}"
        );
    }

    #[test]
    fn detects_import_cycle_indirect() {
        let err = resolve_models(&[
            file("models/a.axm", "import { C } from \"c\"\nexport model A { c: C }"),
            file("models/b.axm", "import { A } from \"a\"\nexport model B { a: A }"),
            file("models/c.axm", "import { B } from \"b\"\nexport model C { b: B }"),
        ])
        .expect_err("cycle");
        assert!(matches!(&err, AxiomError::ModelImportCycle { .. }), "{err}");
    }

    #[test]
    fn acyclic_imports_are_fine() {
        let registry = resolve_models(&[
            file("models/address.axm", "export model Address { street: string }"),
            file(
                "models/user.axm",
                "import { Address } from \"address\"\nexport model User { billing: Address }",
            ),
            file(
                "models/order.axm",
                "import { User } from \"user\"\nexport model Order { owner: User }",
            ),
        ])
        .expect("no cycle");
        assert_eq!(registry.models.len(), 3);
    }
}
