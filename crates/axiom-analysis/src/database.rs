//! The incremental analysis database.
//!
//! This is the heart of the analysis layer: it tracks every file in the
//! workspace, caches parsed artifacts and position indexes keyed by BLAKE3
//! content hash, invalidates only what a change affects, and answers semantic
//! queries. It is deliberately editor-agnostic — both `axiom-cli` and
//! `axiom-lsp` (and any future IDE) use it through this API.
//!
//! Invalidation model:
//!
//! ```text
//! file changed -> hash diff -> reparse that file only
//!                           -> mark catalog/registry dirty
//!                           -> recompute dependent diagnostics
//! ```

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use axiom_core::axm::ast::AxmFile;
use axiom_core::axm::parser::parse_axm_file;
use axiom_core::axm::resolver::{resolve_models, ModelRegistry};
use axiom_core::cache::{compute_content_hash, ToolCache};
use axiom_core::catalog::{parse_sql_catalog, ColumnSchema, TableCatalog, TableSchema};
use axiom_core::config::AxiomConfig;
use axiom_core::errors::AxiomError;
use axiom_core::query::{parse_query_file, QueryCatalog};

use crate::references::{
    query_return_type_refs, resolve_axm_refs, resolve_query_refs, AxmRef, QueryRefs,
};
use crate::symbols::{build_model_symbols, build_table_symbols, SymbolTable};
use crate::token::PositionIndex;

/// Opaque file identifier, following the compiler-database pattern.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FileId(pub u32);#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    Sql,
    Axm,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Schema,
    Query,
    Model,
    Unknown,
}

/// What a single file edit affected. Drives incremental recomputation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ChangeKind {
    /// The file's own AST/tokens changed.
    pub ast: bool,
    /// SQL inputs (schema or query) changed — query diagnostics depend on it.
    pub catalog: bool,
    /// `.axm` inputs changed — the model registry depends on it.
    pub registry: bool,
}

impl ChangeKind {
    pub const NONE: ChangeKind = ChangeKind {
        ast: false,
        catalog: false,
        registry: false,
    };

    pub fn is_none(&self) -> bool {
        !self.ast && !self.catalog && !self.registry
    }
}

pub(crate) struct FileEntry {
    pub(crate) path: PathBuf,
    pub(crate) lang: Lang,
    pub(crate) role: Role,
    pub(crate) text: String,
    pub(crate) hash: [u8; 32],
    pub(crate) index: PositionIndex,
    pub(crate) axm: Option<AxmFile>,
}

/// A lazily-recomputed reference set, keyed by the inputs it depends on.
#[derive(Debug, Clone, Copy)]
struct RefsKey {
    file_hash: [u8; 32],
    symbols_version: u64,
}

/// The incremental analysis database for one workspace.
pub struct AnalysisDatabase {
    pub(crate) files: Vec<FileEntry>,
    by_path: HashMap<PathBuf, usize>,

    /// Per-file owned catalog slices, rebuilt only for the changed file.
    file_catalogs: HashMap<PathBuf, TableCatalog<'static>>,
    /// Per-file owned query catalogs.
    file_query_catalogs: HashMap<PathBuf, QueryCatalog<'static>>,
    /// Lazily-recomputed per-file reference sets.
    query_refs_cache: HashMap<PathBuf, (QueryRefs, RefsKey)>,
    axm_refs_cache: HashMap<PathBuf, (Vec<AxmRef>, RefsKey)>,

    owned_catalog: TableCatalog<'static>,
    catalog_dirty: bool,
    owned_query_catalog: QueryCatalog<'static>,
    query_catalog_dirty: bool,
    registry: Option<ModelRegistry>,
    registry_dirty: bool,

    symbols: SymbolTable,
    symbols_dirty: bool,
    symbols_version: u64,

    /// Cached diagnostics derived from the shared check functions.
    pub(crate) query_diags: Option<Vec<axiom_diagnostics::Diagnostic>>,
    pub(crate) query_diags_dirty: bool,
    pub(crate) model_diags: Option<Vec<axiom_diagnostics::Diagnostic>>,
    pub(crate) model_diags_dirty: bool,

    pub(crate) tools: Option<ToolCache>,
    cache_path: Option<PathBuf>,
}

impl Default for AnalysisDatabase {
    fn default() -> Self {
        Self {
            files: Vec::new(),
            by_path: HashMap::new(),
            file_catalogs: HashMap::new(),
            file_query_catalogs: HashMap::new(),
            query_refs_cache: HashMap::new(),
            axm_refs_cache: HashMap::new(),
            owned_catalog: TableCatalog::default(),
            catalog_dirty: true,
            owned_query_catalog: QueryCatalog::default(),
            query_catalog_dirty: true,
            registry: None,
            registry_dirty: true,
            symbols: SymbolTable::default(),
            symbols_dirty: true,
            symbols_version: 0,
            query_diags: None,
            query_diags_dirty: true,
            model_diags: None,
            model_diags_dirty: true,
            tools: None,
            cache_path: None,
        }
    }
}

impl AnalysisDatabase {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_cache(&mut self, tools: Option<ToolCache>, cache_path: Option<PathBuf>) {
        self.tools = tools;
        self.cache_path = cache_path;
    }

    /// Best-effort persistence of the tools cache. Never fatal.
    pub fn flush_cache(&self) {
        if let (Some(tools), Some(path)) = (&self.tools, &self.cache_path) {
            let _ = tools.save(path);
        }
    }

    // ------------------------------------------------------------------
    // Workspace loading
    // ------------------------------------------------------------------

    /// Load every configured input glob into the database.
    pub fn load_workspace(&mut self, config: &AxiomConfig, base: &Path) -> Result<(), AxiomError> {
        let mut read = |patterns: &[String], role: Role| -> Result<(), AxiomError> {
            for path in axiom_core::config::resolve_glob_paths(patterns, base)? {
                let text = std::fs::read_to_string(&path)?;
                self.open_with_role(&path, text, role);
            }
            Ok(())
        };
        read(&config.inputs.schema, Role::Schema)?;
        read(&config.inputs.queries, Role::Query)?;
        read(&config.inputs.models, Role::Model)?;
        Ok(())
    }

    pub fn file_paths(&self) -> impl Iterator<Item = &Path> {
        self.files.iter().map(|f| f.path.as_path())
    }

    pub fn file_lang(&self, path: &Path) -> Option<Lang> {
        self.files
            .get(self.by_path.get(path).copied()?)
            .map(|f| f.lang)
    }

    pub fn file_role(&self, path: &Path) -> Role {
        self.files
            .get(self.by_path.get(path).copied().unwrap_or(usize::MAX))
            .map(|f| f.role)
            .unwrap_or(Role::Unknown)
    }

    pub fn file_text(&self, path: &Path) -> Option<&str> {
        self.files
            .get(self.by_path.get(path).copied()?)
            .map(|f| f.text.as_str())
    }

    pub fn is_open(&self, path: &Path) -> bool {
        self.by_path.contains_key(path)
    }

    fn entry_mut(&mut self, path: &Path) -> Option<&mut FileEntry> {
        let idx = *self.by_path.get(path)?;
        self.files.get_mut(idx)
    }

    /// Insert or update a file's contents. Returns what changed, if anything.
    pub fn open(&mut self, path: &Path, text: String) -> ChangeKind {
        self.open_with_role(path, text, Role::Unknown)
    }

    fn open_with_role(&mut self, path: &Path, text: String, role: Role) -> ChangeKind {
        let hash = compute_content_hash(text.as_bytes());

        if let Some(idx) = self.by_path.get(path).copied() {
            if self.files[idx].hash == hash {
                return ChangeKind::NONE;
            }
            let (lang, role) = {
                let entry = &self.files[idx];
                (entry.lang, entry.role)
            };
            let index = self.build_index(&lang, &text);
            let axm = if lang == Lang::Axm {
                parse_axm_file(&text).ok()
            } else {
                None
            };
            let entry = &mut self.files[idx];
            entry.text = text;
            entry.hash = hash;
            entry.index = index;
            entry.axm = axm;
            self.on_parsed(path, lang, role);
            return ChangeKind {
                ast: true,
                catalog: matches!(role, Role::Schema | Role::Query),
                registry: lang == Lang::Axm,
            };
        }

        let lang = lang_for_path(path);
        let index = self.build_index(&lang, &text);
        let axm = if lang == Lang::Axm { parse_axm_file(&text).ok() } else { None };
        self.by_path.insert(path.to_path_buf(), self.files.len());
        self.files.push(FileEntry {
            path: path.to_path_buf(),
            lang,
            role,
            text,
            hash,
            index,
            axm,
        });
        self.on_parsed(path, lang, role);
        ChangeKind {
            ast: true,
            catalog: matches!(role, Role::Schema | Role::Query),
            registry: lang == Lang::Axm,
        }
    }

    fn build_index(&self, lang: &Lang, text: &str) -> PositionIndex {
        match lang {
            Lang::Sql => PositionIndex::new_sql(text),
            Lang::Axm => PositionIndex::new_axm(text),
        }
    }

    /// Recompute the per-file artifacts after its text changed.
    fn on_parsed(&mut self, path: &Path, lang: Lang, role: Role) {
        let Some(entry) = self.entry_mut(path) else {
            return;
        };
        let text = entry.text.clone();
        let index = entry.index.clone();
        let axm = entry.axm.clone();

        match lang {
            Lang::Sql => {
                let catalog = parse_sql_catalog(&text)
                    .ok()
                    .map(|c| own_catalog(&c))
                    .unwrap_or_default();
                self.file_catalogs.insert(path.to_path_buf(), catalog);
                let queries = parse_query_file(&text)
                    .ok()
                    .map(|q| own_query_catalog(&q))
                    .unwrap_or_default();
                self.file_query_catalogs.insert(path.to_path_buf(), queries);
            }
            Lang::Axm => {
                // The axm refs are recomputed lazily against the symbol table.
                let _ = (&text, &index, &axm);
            }
        }

        match role {
            Role::Schema | Role::Query => {
                self.catalog_dirty = true;
                self.query_catalog_dirty = true;
                self.query_diags_dirty = true;
            }
            Role::Model => {
                self.registry_dirty = true;
                self.model_diags_dirty = true;
            }
            Role::Unknown => {}
        }
        self.symbols_dirty = true;
    }

    /// Remove a file from the database (e.g. on `didClose`). References from
    /// other files are no longer resolved against it.
    pub fn close(&mut self, path: &Path) {
        if let Some(idx) = self.by_path.remove(path) {
            self.files.remove(idx);
            self.by_path = self
                .files
                .iter()
                .enumerate()
                .map(|(i, f)| (f.path.clone(), i))
                .collect();
        }
        self.file_catalogs.remove(path);
        self.file_query_catalogs.remove(path);
        self.query_refs_cache.remove(path);
        self.axm_refs_cache.remove(path);
        self.symbols_dirty = true;
        self.query_diags_dirty = true;
        self.model_diags_dirty = true;
        self.catalog_dirty = true;
        self.registry_dirty = true;
    }

    /// All files whose diagnostics depend on `path`'s change.
    pub fn affected_files(&self, changed: &Path, change: &ChangeKind) -> Vec<PathBuf> {
        let mut out = vec![changed.to_path_buf()];
        let mut seen = std::collections::HashSet::new();
        seen.insert(changed.to_path_buf());
        if change.catalog {
            for entry in &self.files {
                if entry.role == Role::Query && entry.path != changed && seen.insert(entry.path.clone()) {
                    out.push(entry.path.clone());
                }
            }
        }
        if change.registry {
            for entry in &self.files {
                if entry.role == Role::Model && entry.path != changed && seen.insert(entry.path.clone()) {
                    out.push(entry.path.clone());
                }
            }
        }
        out
    }

    // ------------------------------------------------------------------
    // Merged views (rebuilt lazily, only when dirty)
    // ------------------------------------------------------------------

    pub fn catalog(&mut self) -> &TableCatalog<'static> {
        if self.catalog_dirty {
            let tables: Vec<TableSchema<'static>> = self
                .file_catalogs
                .values()
                .flat_map(|c| c.tables.iter().cloned())
                .collect();
            self.owned_catalog = TableCatalog { tables };
            self.catalog_dirty = false;
        }
        &self.owned_catalog
    }

    pub fn query_catalog(&mut self) -> &QueryCatalog<'static> {
        if self.query_catalog_dirty {
            let queries: Vec<axiom_core::query::QueryDefinition<'static>> = self
                .file_query_catalogs
                .values()
                .flat_map(|c| c.queries.iter().cloned())
                .collect();
            self.owned_query_catalog = QueryCatalog { queries };
            self.query_catalog_dirty = false;
        }
        &self.owned_query_catalog
    }

    pub fn registry(&mut self) -> Option<&ModelRegistry> {
        if self.registry_dirty {
            let files: Vec<(PathBuf, String)> = self
                .files
                .iter()
                .filter(|f| f.role == Role::Model)
                .map(|f| (f.path.clone(), f.text.clone()))
                .collect();
            self.registry = if files.is_empty() {
                None
            } else {
                resolve_models(&files).ok()
            };
            self.registry_dirty = false;
        }
        self.registry.as_ref()
    }

    /// The workspace symbol table, rebuilt only when dirty.
    pub fn symbol_table(&mut self) -> &SymbolTable {
        if self.symbols_dirty {
            let mut symbols = SymbolTable::default();

            let table_inputs: Vec<(PathBuf, TableCatalog<'static>, PositionIndex)> = self
                .file_catalogs
                .iter()
                .filter_map(|(path, catalog)| {
                    self.by_path.get(path).map(|&idx| {
                        let entry = &self.files[idx];
                        (path.clone(), catalog.clone(), entry.index.clone())
                    })
                })
                .collect();
            for (path, catalog, index) in table_inputs {
                for table in build_table_symbols(&path, &catalog, &index) {
                    symbols.add_table(table);
                }
            }

            for entry in &self.files {
                if entry.lang == Lang::Axm
                    && let Some(axm) = &entry.axm
                {
                    for model in build_model_symbols(&entry.path, axm, &entry.index) {
                        symbols.add_model(model);
                    }
                }
            }

            self.symbols = symbols;
            self.symbols_dirty = false;
            self.symbols_version += 1;
        }
        &self.symbols
    }

    /// SQL references for a query file, recomputed only when the file or the
    /// symbol table changed.
    pub fn query_refs(&mut self, path: &Path) -> Option<QueryRefs> {
        let entry = self.files.get(self.by_path.get(path).copied()?)?;
        let hash = entry.hash;
        let text = entry.text.clone();
        let index = entry.index.clone();
        let symbols_version = self.symbols_version;

        let key = RefsKey {
            file_hash: hash,
            symbols_version,
        };
        if let Some((refs, cached)) = self.query_refs_cache.get(path)
            && !self.symbols_dirty
            && cached.file_hash == key.file_hash
            && cached.symbols_version == key.symbols_version
        {
            return Some(refs.clone());
        }
        let symbols = self.symbol_table();
        let refs = resolve_query_refs(path, &text, &index, symbols);
        self.query_refs_cache.insert(
            path.to_path_buf(),
            (
                refs.clone(),
                RefsKey {
                    file_hash: hash,
                    symbols_version: self.symbols_version,
                },
            ),
        );
        Some(refs)
    }

    /// AXM references for a model file, recomputed lazily.
    pub fn axm_refs(&mut self, path: &Path) -> Vec<AxmRef> {
        let Some(idx) = self.by_path.get(path).copied() else {
            return Vec::new();
        };
        let Some(entry) = self.files.get(idx) else {
            return Vec::new();
        };
        let hash = entry.hash;
        let Some(axm) = entry.axm.clone() else {
            return Vec::new();
        };
        let text = entry.text.clone();
        let index = entry.index.clone();
        let symbols_version = self.symbols_version;

        let key = RefsKey {
            file_hash: hash,
            symbols_version,
        };
        if let Some((refs, cached)) = self.axm_refs_cache.get(path)
            && !self.symbols_dirty
            && cached.file_hash == key.file_hash
            && cached.symbols_version == key.symbols_version
        {
            return refs.clone();
        }
        let symbols = self.symbol_table();
        let refs = resolve_axm_refs(path, &text, &index, &axm, symbols);
        self.axm_refs_cache.insert(
            path.to_path_buf(),
            (
                refs.clone(),
                RefsKey {
                    file_hash: hash,
                    symbols_version: self.symbols_version,
                },
            ),
        );
        refs
    }

    /// Return-type references (`: User`) found in a query file's annotations.
    pub fn return_type_refs(&mut self, path: &Path) -> Vec<AxmRef> {
        let Some((_hash, text, _index)) = self.file_data(path) else {
            return Vec::new();
        };
        if self.file_lang(path) != Some(Lang::Sql) {
            return Vec::new();
        }
        if let Some(queries) = self.file_query_catalogs.get(path) {
            return query_return_type_refs(path, text, queries);
        }
        Vec::new()
    }

    /// (hash, text, index) for a file, if known.
    pub(crate) fn file_data(&self, path: &Path) -> Option<([u8; 32], &str, &PositionIndex)> {
        let entry = self.files.get(self.by_path.get(path).copied()?)?;
        Some((entry.hash, &entry.text, &entry.index))
    }

    /// The position index for a file, if known.
    pub fn position_index(&self, path: &Path) -> Option<&PositionIndex> {
        self.files
            .get(self.by_path.get(path).copied()?)
            .map(|f| &f.index)
    }

    /// The parsed `.axm` AST for a model file, if any.
    pub fn axm_file(&self, path: &Path) -> Option<&AxmFile> {
        self.files
            .get(self.by_path.get(path).copied()?)
            .and_then(|f| f.axm.as_ref())
    }
}

fn lang_for_path(path: &Path) -> Lang {
    match path.extension().and_then(|e| e.to_str()) {
        Some("sql") => Lang::Sql,
        Some("axm") => Lang::Axm,
        _ => Lang::Sql,
    }
}

fn own_catalog(catalog: &TableCatalog) -> TableCatalog<'static> {
    let tables = catalog
        .tables
        .iter()
        .map(|t| TableSchema {
            name: t.name.to_string().into(),
            columns: t
                .columns
                .iter()
                .map(|c| ColumnSchema {
                    name: c.name.to_string().into(),
                    data_type: c.data_type.to_string().into(),
                    nullable: c.nullable,
                    primary_key: c.primary_key,
                    rules: Vec::new(),
                })
                .collect(),
        })
        .collect();
    TableCatalog { tables }
}

fn own_query_catalog(catalog: &QueryCatalog) -> QueryCatalog<'static> {
    use axiom_core::query::{QueryDefinition, QueryParam, QueryReturnType};
    let queries = catalog
        .queries
        .iter()
        .map(|q| QueryDefinition {
            name: q.name.to_string().into(),
            sql: q.sql.clone(),
            params: q
                .params
                .iter()
                .map(|p| QueryParam {
                    name: p.name.to_string().into(),
                    param_type: p.param_type.to_string().into(),
                })
                .collect(),
            return_type: match &q.return_type {
                QueryReturnType::Single(n) => QueryReturnType::Single(n.to_string().into()),
                QueryReturnType::Many(n) => QueryReturnType::Many(n.to_string().into()),
                QueryReturnType::Exec => QueryReturnType::Exec,
            },
            validations: q
                .validations
                .iter()
                .map(|(k, v)| {
                    let owned: Vec<_> = v
                        .iter()
                        .map(|rule| axiom_core::catalog::ValidationRule {
                            kind: own_rule_kind(&rule.kind),
                            custom_message: rule
                                .custom_message
                                .as_ref()
                                .map(|m| std::borrow::Cow::Owned(m.to_string())),
                        })
                        .collect();
                    (k.to_string().into(), owned)
                })
                .collect(),
        })
        .collect();
    QueryCatalog { queries }
}

fn own_rule_kind(kind: &axiom_core::catalog::RuleKind) -> axiom_core::catalog::RuleKind<'static> {
    use axiom_core::catalog::RuleKind;
    match kind {
        RuleKind::Regex(p) => RuleKind::Regex(std::borrow::Cow::Owned(p.to_string())),
        RuleKind::MinLen(n) => RuleKind::MinLen(*n),
        RuleKind::MaxLen(n) => RuleKind::MaxLen(*n),
        RuleKind::Min(n) => RuleKind::Min(*n),
        RuleKind::Max(n) => RuleKind::Max(*n),
        RuleKind::Email => RuleKind::Email,
        RuleKind::Url => RuleKind::Url,
        RuleKind::Uuid => RuleKind::Uuid,
        RuleKind::Ulid => RuleKind::Ulid,
        RuleKind::Ipv4 => RuleKind::Ipv4,
        RuleKind::Ipv6 => RuleKind::Ipv6,
        RuleKind::IsoDate => RuleKind::IsoDate,
        RuleKind::Alphanumeric => RuleKind::Alphanumeric,
        RuleKind::Trim => RuleKind::Trim,
        RuleKind::LowerCase => RuleKind::LowerCase,
        RuleKind::UpperCase => RuleKind::UpperCase,
    }
}
