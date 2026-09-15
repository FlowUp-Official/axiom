//! `.axm` import resolution and cross-file linking.
//!
//! The resolver:
//!
//! * parses every source file,
//! * detects duplicate names (models/types share one namespace; queries have
//!   their own),
//! * links `import { A as B } from "path"` statements to concrete files,
//! * verifies every referenced type is in scope,
//! * detects import cycles with a diagnostic chain.
//!
//! The resulting [`ModelRegistry`] is a flattened, deterministic view of every
//! declaration that the code generators emit, plus the alias map (written name
//! -> canonical declaration name) introduced by `import { User as DbUser }`.
//!
//! Axiom identifiers are strictly case-sensitive, and the resolver never
//! canonicalizes them: `User` and `user` are distinct names.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::axm::ast::{AnnotatedType, AxmFile, ModelDecl, QueryDecl, TypeDecl, TypeRef};
use crate::axm::parser::parse_axm_file;
use crate::errors::AxiomError;

/// A model together with the file it was declared in.
#[derive(Debug, Clone)]
pub struct ResolvedModel {
    pub path: PathBuf,
    pub model: ModelDecl,
}

/// A type alias together with the file it was declared in.
#[derive(Debug, Clone)]
pub struct ResolvedType {
    pub path: PathBuf,
    pub ty: TypeDecl,
}

/// A query together with the file it was declared in.
#[derive(Debug, Clone)]
pub struct ResolvedQuery {
    pub path: PathBuf,
    pub query: QueryDecl,
}

/// The linked set of all declarations across a group of `.axm` files.
#[derive(Debug, Default)]
pub struct ModelRegistry {
    /// Models in file/declaration order.
    pub models: Vec<ResolvedModel>,
    /// Model name -> index into [`ModelRegistry::models`].
    pub index: BTreeMap<String, usize>,
    /// Type aliases in file/declaration order.
    pub types: Vec<ResolvedType>,
    /// Type alias name -> index into [`ModelRegistry::types`].
    pub type_index: BTreeMap<String, usize>,
    /// Queries in file/declaration order.
    pub queries: Vec<ResolvedQuery>,
    /// Query name -> index into [`ModelRegistry::queries`].
    pub query_index: BTreeMap<String, usize>,
    /// For each canonical file path, the written (possibly aliased) name as it
    /// appears in scope -> the canonical declaration name. e.g.
    /// `import { User as DbUser }` records `DbUser -> User`.
    pub aliases: BTreeMap<PathBuf, BTreeMap<String, String>>,
}

impl ModelRegistry {
    pub fn is_empty(&self) -> bool {
        self.models.is_empty() && self.types.is_empty() && self.queries.is_empty()
    }

    pub fn model_by_name(&self, name: &str) -> Option<&ResolvedModel> {
        self.index.get(name).map(|&i| &self.models[i])
    }

    pub fn type_by_name(&self, name: &str) -> Option<&ResolvedType> {
        self.type_index.get(name).map(|&i| &self.types[i])
    }

    pub fn query_by_name(&self, name: &str) -> Option<&ResolvedQuery> {
        self.query_index.get(name).map(|&i| &self.queries[i])
    }

    /// The canonical declaration name for a written name in a file, resolving
    /// import aliases (`DbUser` -> `User`). Names that are not aliased resolve
    /// to themselves.
    pub fn effective_name<'a>(&'a self, path: &Path, written: &'a str) -> &'a str {
        self.aliases
            .get(&canonical(path))
            .and_then(|m| m.get(written))
            .map(|s| s.as_str())
            .unwrap_or(written)
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

    // Canonical path -> file index, for import resolution.
    let mut canonical_index: BTreeMap<PathBuf, usize> = BTreeMap::new();
    for (idx, (path, _)) in files.iter().enumerate() {
        canonical_index.insert(canonical(path), idx);
    }

    // Resolve imports. `bindings` records (file_idx, written_name, canonical).
    let mut bindings: Vec<(usize, String, String)> = Vec::new();
    let mut edges: Vec<(usize, usize)> = Vec::new();
    let mut per_file_bindings: BTreeMap<usize, BTreeMap<String, String>> = BTreeMap::new();
    for (file_idx, (path, file)) in files.iter().enumerate() {
        let mut written_names: BTreeSet<String> = BTreeSet::new();
        for import in &file.imports {
            let target = resolve_import_path(path, &import.source);
            let Some(&target_idx) = canonical_index.get(&canonical(&target)) else {
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
                let target_file = &files[target_idx].1;
                let canonical_name = if target_file.model_by_name(&name.name).is_some() {
                    name.name.clone()
                } else if target_file.type_by_name(&name.name).is_some() {
                    name.name.clone()
                } else {
                    return Err(AxiomError::ModelResolutionError {
                        path: path.clone(),
                        message: format!(
                            "imported symbol `{}` does not exist in `{}`",
                            name.name, import.source
                        ),
                    });
                };
                let written = name.alias.clone().unwrap_or_else(|| name.name.clone());
                if !written_names.insert(written.clone()) {
                    return Err(AxiomError::ModelResolutionError {
                        path: path.clone(),
                        message: format!(
                            "duplicate import binding `{written}` in `{}`",
                            path.display()
                        ),
                    });
                }
                if local_type_names(file).contains(&written) {
                    return Err(AxiomError::ModelResolutionError {
                        path: path.clone(),
                        message: format!(
                            "imported name `{written}` collides with a locally declared model or type in `{}`",
                            path.display()
                        ),
                    });
                }
                bindings.push((file_idx, written.clone(), canonical_name));
            }
        }
        per_file_bindings.insert(
            file_idx,
            bindings
                .iter()
                .filter(|(i, _, _)| *i == file_idx)
                .map(|(_, w, c)| (w.clone(), c.clone()))
                .collect(),
        );
    }

    detect_cycles(&files, &edges)?;

    // Duplicate detection. Models and type aliases share one namespace (they
    // are both compile-time types); queries are their own function namespace.
    let mut type_names: BTreeMap<String, usize> = BTreeMap::new();
    for (file_idx, (path, file)) in files.iter().enumerate() {
        for name in file
            .types
            .iter()
            .map(|t| t.name.as_str())
            .chain(file.models.iter().map(|m| m.name.as_str()))
        {
            if let Some(prev) = type_names.get(name) {
                return Err(AxiomError::ModelDuplicate {
                    name: name.to_string(),
                    first: files[*prev].0.display().to_string(),
                    second: path.display().to_string(),
                });
            }
            type_names.insert(name.to_string(), file_idx);
        }
    }

    let mut query_names: BTreeMap<String, usize> = BTreeMap::new();
    for (file_idx, (path, file)) in files.iter().enumerate() {
        for query in &file.queries {
            if let Some(prev) = query_names.get(&query.name) {
                return Err(AxiomError::ModelDuplicate {
                    name: query.name.clone(),
                    first: files[*prev].0.display().to_string(),
                    second: path.display().to_string(),
                });
            }
            query_names.insert(query.name.clone(), file_idx);
        }
    }

    // Every named type reference must be in scope: declared locally or
    // imported.
    for (file_idx, (path, file)) in files.iter().enumerate() {
        let mut allowed: BTreeSet<String> = local_type_names(file);
        for (i, w, _) in bindings.iter().filter(|(i, _, _)| *i == file_idx) {
            let _ = i;
            allowed.insert(w.clone());
        }
        let scope = &allowed;
        for ty in &file.types {
            check_annotated(&ty.ty, scope, &format!("type `{}`", ty.name), path)?;
        }
        for model in &file.models {
            for field in &model.fields {
                check_annotated(
                    &field.ty,
                    scope,
                    &format!("field `{}` of model `{}`", field.name, model.name),
                    path,
                )?;
            }
        }
        for query in &file.queries {
            for param in &query.params {
                check_type_refs(
                    &param.ty,
                    scope,
                    &format!("parameter `{}` of query `{}`", param.name, query.name),
                    path,
                )?;
            }
            match &query.return_type {
                crate::axm::ast::QueryReturn::Exec => {}
                crate::axm::ast::QueryReturn::Single(ty)
                | crate::axm::ast::QueryReturn::Optional(ty)
                | crate::axm::ast::QueryReturn::Many(ty) => check_type_refs(
                    ty,
                    scope,
                    &format!("return type of query `{}`", query.name),
                    path,
                )?,
            }
        }
    }

    let mut registry = ModelRegistry::default();
    for (path, file) in files.iter() {
        for ty in &file.types {
            let pos = registry.types.len();
            registry.type_index.insert(ty.name.clone(), pos);
            registry.types.push(ResolvedType {
                path: path.clone(),
                ty: ty.clone(),
            });
        }
        for model in &file.models {
            let pos = registry.models.len();
            registry.index.insert(model.name.clone(), pos);
            registry.models.push(ResolvedModel {
                path: path.clone(),
                model: model.clone(),
            });
        }
        for query in &file.queries {
            let pos = registry.queries.len();
            registry.query_index.insert(query.name.clone(), pos);
            registry.queries.push(ResolvedQuery {
                path: path.clone(),
                query: query.clone(),
            });
        }
    }
    for file_idx in 0..files.len() {
        if let Some(bindings) = per_file_bindings.remove(&file_idx) {
            let path = &files[file_idx].0;
            registry.aliases.insert(canonical(path), bindings);
        }
    }

    Ok(registry)
}

/// Local model and type names declared in a file.
fn local_type_names(file: &AxmFile) -> BTreeSet<String> {
    file.types
        .iter()
        .map(|t| t.name.clone())
        .chain(file.models.iter().map(|m| m.name.clone()))
        .collect()
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

fn check_annotated(
    ty: &AnnotatedType,
    scope: &BTreeSet<String>,
    decl: &str,
    path: &Path,
) -> Result<(), AxiomError> {
    check_type_refs(&ty.base, scope, decl, path)
}

fn check_type_refs(
    ty: &TypeRef,
    scope: &BTreeSet<String>,
    decl: &str,
    path: &Path,
) -> Result<(), AxiomError> {
    match ty {
        TypeRef::Array(inner) => check_type_refs(inner, scope, decl, path),
        TypeRef::Nullable(inner) => check_type_refs(inner, scope, decl, path),
        TypeRef::Named(name) => {
            if scope.contains(name) {
                Ok(())
            } else {
                Err(AxiomError::ModelResolutionError {
                    path: path.to_path_buf(),
                    message: format!(
                        "unknown type `{name}` in {decl} \
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
    fn links_imported_declarations_across_files() {
        let registry = resolve_models(&[
            file("models/address.axm", "model Address { street: String }"),
            file(
                "models/user.axm",
                "import { Address } from \"address\"\nmodel User { billing: Address }",
            ),
        ])
        .expect("resolve");
        assert_eq!(registry.models.len(), 2);
        assert!(registry.model_by_name("Address").is_some());
        assert!(registry.model_by_name("User").is_some());
    }

    #[test]
    fn imported_types_are_linked() {
        let registry = resolve_models(&[
            file("models/shared.axm", "type Email = String.email();"),
            file(
                "models/user.axm",
                "import { Email } from \"shared\"\nmodel User { email: Email }",
            ),
        ])
        .expect("resolve");
        assert_eq!(registry.types.len(), 1);
        assert!(registry.type_by_name("Email").is_some());
    }

    #[test]
    fn aliased_imports_record_effective_names() {
        let registry = resolve_models(&[
            file("models/address.axm", "model Address { street: String }"),
            file(
                "models/user.axm",
                "import { Address as DbAddress } from \"address\"\nmodel User { home: DbAddress }",
            ),
        ])
        .expect("resolve");
        let alias = registry
            .aliases
            .get(&canonical(Path::new("models/user.axm")))
            .expect("alias map");
        assert_eq!(alias.get("DbAddress"), Some(&"Address".to_string()));
        assert_eq!(
            registry.effective_name(Path::new("models/user.axm"), "DbAddress"),
            "Address"
        );
        assert_eq!(registry.effective_name(Path::new("models/user.axm"), "home"), "home");
    }

    #[test]
    fn detects_unresolvable_import() {
        let err = resolve_models(&[file(
            "models/user.axm",
            "import { Address } from \"missing\"\nmodel User { billing: Address }",
        )])
        .expect_err("missing file");
        assert!(
            matches!(&err, AxiomError::ModelResolutionError { message, .. }
                if message.contains("cannot resolve import")),
            "{err}"
        );
    }

    #[test]
    fn detects_import_of_nonexistent_symbol() {
        let err = resolve_models(&[
            file("models/address.axm", "model Address { street: String }"),
            file(
                "models/user.axm",
                "import { Nope } from \"address\"\nmodel User { }",
            ),
        ])
        .expect_err("missing symbol");
        assert!(
            matches!(&err, AxiomError::ModelResolutionError { message, .. }
                if message.contains("does not exist")),
            "{err}"
        );
    }

    #[test]
    fn detects_import_colliding_with_local_declaration() {
        let err = resolve_models(&[
            file("models/address.axm", "model Address { street: String }"),
            file(
                "models/user.axm",
                "import { Address } from \"address\"\nmodel Address { x: String }",
            ),
        ])
        .expect_err("collision");
        assert!(
            matches!(&err, AxiomError::ModelResolutionError { message, .. }
                if message.contains("collides")),
            "{err}"
        );
    }

    #[test]
    fn detects_duplicate_model_names() {
        let err = resolve_models(&[
            file("models/a.axm", "model User { a: String }"),
            file("models/b.axm", "model User { b: String }"),
        ])
        .expect_err("duplicate");
        assert!(matches!(&err, AxiomError::ModelDuplicate { name, .. } if name == "User"), "{err}");
    }

    #[test]
    fn detects_type_and_model_name_overlap() {
        let err = resolve_models(&[
            file("models/a.axm", "type User = String;"),
            file("models/b.axm", "model User { b: String }"),
        ])
        .expect_err("duplicate");
        assert!(matches!(&err, AxiomError::ModelDuplicate { name, .. } if name == "User"), "{err}");
    }

    #[test]
    fn detects_duplicate_query_names() {
        let err = resolve_models(&[
            file("models/a.axm", "query GetUser($id: UUID) -> User? {\n  SELECT * FROM users WHERE id = $id;\n}"),
            file("models/b.axm", "query GetUser($id: UUID) -> User? {\n  SELECT * FROM users WHERE id = $id;\n}"),
        ])
        .expect_err("duplicate");
        assert!(matches!(&err, AxiomError::ModelDuplicate { name, .. } if name == "GetUser"), "{err}");
    }

    #[test]
    fn resolves_type_across_namespaces() {
        let registry = resolve_models(&[
            file("models/a.axm", "type Email = String.email();"),
            file(
                "models/b.axm",
                "import { Email } from \"a\"\nmodel User { id: UUID }\nquery GetByEmail($email: Email) -> User? {\n  SELECT * FROM users WHERE email = $email;\n}",
            ),
        ])
        .expect("linked");
        assert!(registry.type_by_name("Email").is_some());
    }

    #[test]
    fn detects_unreferenced_type() {
        let err = resolve_models(&[file(
            "models/user.axm",
            "model User { billing: Address }",
        )])
        .expect_err("unknown type");
        assert!(
            matches!(&err, AxiomError::ModelResolutionError { message, .. }
                if message.contains("unknown type `Address`")),
            "{err}"
        );
    }

    #[test]
    fn case_sensitivity_is_preserved() {
        // `User` referenced as `user` must be flagged as unknown, exactly like
        // an undeclared name.
        let err = resolve_models(&[
            file("models/a.axm", "model User { id: String }"),
            file("models/b.axm", "model Delegate { owner: user }"),
        ])
        .expect_err("unknown type");
        assert!(
            matches!(&err, AxiomError::ModelResolutionError { message, .. }
                if message.contains("unknown type `user`")),
            "{err}"
        );
    }

    #[test]
    fn detects_import_cycle_direct() {
        let err = resolve_models(&[
            file("models/a.axm", "import { B } from \"b\"\nmodel A { b: B }"),
            file("models/b.axm", "import { A } from \"a\"\nmodel B { a: A }"),
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
            file("models/a.axm", "import { C } from \"c\"\nmodel A { c: C }"),
            file("models/b.axm", "import { A } from \"a\"\nmodel B { a: A }"),
            file("models/c.axm", "import { B } from \"b\"\nmodel C { b: B }"),
        ])
        .expect_err("cycle");
        assert!(matches!(&err, AxiomError::ModelImportCycle { .. }), "{err}");
    }

    #[test]
    fn acyclic_imports_are_fine() {
        let registry = resolve_models(&[
            file("models/address.axm", "model Address { street: String }"),
            file(
                "models/user.axm",
                "import { Address } from \"address\"\nmodel User { billing: Address }",
            ),
            file(
                "models/order.axm",
                "import { User } from \"user\"\nmodel Order { owner: User }",
            ),
        ])
        .expect("no cycle");
        assert_eq!(registry.models.len(), 3);
    }
}