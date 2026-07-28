//! Bounded declarative build-context imports for supported language families.
//!
//! The schema captures source-free project metadata without executing repository
//! code and derives a deterministic identity only after strict normalization.

use rootlight_ids::content_hash;
use rootlight_ir::BuildContextIdentity;
use serde::{Deserialize, Serialize};

/// Current schema version accepted by [`ProjectContext::decode_json`].
pub const PROJECT_CONTEXT_SCHEMA_VERSION: u16 = 1;
/// Hard ceiling for one encoded project-context manifest.
pub const MAX_PROJECT_CONTEXT_BYTES: usize = 1024 * 1024;
/// Hard ceiling for all repeated values in one project-context manifest.
pub const MAX_PROJECT_CONTEXT_ITEMS: usize = 8_192;
/// Hard ceiling for one string in a project-context manifest.
pub const MAX_PROJECT_CONTEXT_STRING_BYTES: usize = 4_096;

const IDENTITY_DOMAIN: &[u8] = b"rootlight-project-context-v1\0";

/// Language identity carried by a declarative project context.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ProjectContextLanguage {
    /// Rust source interpreted with Cargo workspace and target metadata.
    Rust,
    /// TypeScript source interpreted with a `tsconfig` project graph.
    Typescript,
    /// JavaScript source interpreted with a `tsconfig` or `jsconfig` project graph.
    Javascript,
    /// Python source interpreted with package-root and type-checker metadata.
    Python,
    /// Go source interpreted with module, workspace, and build-tag metadata.
    Go,
    /// C source interpreted with a compile database.
    C,
    /// C++ source interpreted with a compile database.
    Cpp,
    /// Java source interpreted with a JVM project model.
    Java,
    /// Kotlin source interpreted with a JVM project model.
    Kotlin,
    /// C# source interpreted with a .NET project model.
    Csharp,
    /// PHP source interpreted with Composer metadata.
    Php,
}

/// Explicit completeness declared for imported project metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ProjectContextCoverageStatus {
    /// Every required project input was captured for the declared target.
    Complete,
    /// A documented hard limit excluded some inputs.
    Bounded,
    /// Only a documented sample of inputs was captured.
    Sampled,
    /// Completeness could not be established.
    Unknown,
}

/// One source-free explanation for incomplete project metadata.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectContextSkip {
    code: String,
    detail: String,
}

impl ProjectContextSkip {
    /// Returns the stable lowercase `snake_case` reason code.
    #[must_use]
    pub fn code(&self) -> &str {
        &self.code
    }

    /// Returns the bounded source-free explanation.
    #[must_use]
    pub fn detail(&self) -> &str {
        &self.detail
    }
}

/// Coverage declaration attached to one project context.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectContextCoverage {
    status: ProjectContextCoverageStatus,
    skips: Vec<ProjectContextSkip>,
}

impl ProjectContextCoverage {
    /// Returns the declared completeness.
    #[must_use]
    pub const fn status(&self) -> ProjectContextCoverageStatus {
        self.status
    }

    /// Returns skip reasons in canonical code order.
    #[must_use]
    pub fn skips(&self) -> &[ProjectContextSkip] {
        &self.skips
    }
}

/// One exact generated span and its project-relative handwritten origin.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GeneratedOriginMapping {
    generated_start_byte: u64,
    generated_end_byte: u64,
    origin_path: String,
    origin_start_byte: u64,
    origin_end_byte: u64,
    transformation: String,
    generator_digest: Option<String>,
}

impl GeneratedOriginMapping {
    /// Returns the generated half-open byte range.
    #[must_use]
    pub const fn generated_range(&self) -> (u64, u64) {
        (self.generated_start_byte, self.generated_end_byte)
    }

    /// Returns the normalized project-relative origin path.
    #[must_use]
    pub fn origin_path(&self) -> &str {
        &self.origin_path
    }

    /// Returns the origin half-open byte range.
    #[must_use]
    pub const fn origin_range(&self) -> (u64, u64) {
        (self.origin_start_byte, self.origin_end_byte)
    }

    /// Returns the stable transformation identity.
    #[must_use]
    pub fn transformation(&self) -> &str {
        &self.transformation
    }

    /// Returns the generator or schema digest when supplied.
    #[must_use]
    pub fn generator_digest(&self) -> Option<&str> {
        self.generator_digest.as_deref()
    }
}

/// One project-relative source file and its optional generated origin.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectContextFile {
    path: String,
    generated_from: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    origin_mappings: Vec<GeneratedOriginMapping>,
}

impl ProjectContextFile {
    /// Returns the normalized project-relative source path.
    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }

    /// Returns the recorded origin for a generated source when known.
    #[must_use]
    pub fn generated_from(&self) -> Option<&str> {
        self.generated_from.as_deref()
    }

    /// Returns canonical generated-to-origin span mappings.
    #[must_use]
    pub fn origin_mappings(&self) -> &[GeneratedOriginMapping] {
        &self.origin_mappings
    }
}

/// One metadata value and the declarative source that established it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectContextEvidence {
    value: String,
    source: String,
}

impl ProjectContextEvidence {
    /// Returns the captured metadata value.
    #[must_use]
    pub fn value(&self) -> &str {
        &self.value
    }

    /// Returns the declarative file or provider that established the value.
    #[must_use]
    pub fn source(&self) -> &str {
        &self.source
    }
}

/// Rust Cargo workspace and target context.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RustProjectContext {
    cargo_metadata_digest: String,
    cargo_lock_digest: Option<String>,
    workspace_members: Vec<String>,
    targets: Vec<String>,
    source_roots: Vec<String>,
    edition: String,
    target_triple: String,
    enabled_features: Vec<String>,
    cfgs: Vec<String>,
    compiler_version: Option<String>,
    precise_index_digest: Option<String>,
}

impl RustProjectContext {
    /// Returns the digest of normalized Cargo metadata.
    #[must_use]
    pub fn cargo_metadata_digest(&self) -> &str {
        &self.cargo_metadata_digest
    }

    /// Returns the captured Cargo lockfile digest, if one exists.
    #[must_use]
    pub fn cargo_lock_digest(&self) -> Option<&str> {
        self.cargo_lock_digest.as_deref()
    }

    /// Returns canonical Cargo workspace members.
    #[must_use]
    pub fn workspace_members(&self) -> &[String] {
        &self.workspace_members
    }

    /// Returns canonical package targets.
    #[must_use]
    pub fn targets(&self) -> &[String] {
        &self.targets
    }

    /// Returns project-relative Rust source roots.
    #[must_use]
    pub fn source_roots(&self) -> &[String] {
        &self.source_roots
    }

    /// Returns the Rust edition selected for the target.
    #[must_use]
    pub fn edition(&self) -> &str {
        &self.edition
    }

    /// Returns the exact compilation target triple.
    #[must_use]
    pub fn target_triple(&self) -> &str {
        &self.target_triple
    }

    /// Returns the enabled Cargo features.
    #[must_use]
    pub fn enabled_features(&self) -> &[String] {
        &self.enabled_features
    }

    /// Returns normalized active `cfg` predicates.
    #[must_use]
    pub fn cfgs(&self) -> &[String] {
        &self.cfgs
    }

    /// Returns the compiler frontend identity when captured.
    #[must_use]
    pub fn compiler_version(&self) -> Option<&str> {
        self.compiler_version.as_deref()
    }

    /// Returns the compiler-precise index digest when captured.
    #[must_use]
    pub fn precise_index_digest(&self) -> Option<&str> {
        self.precise_index_digest.as_deref()
    }
}

/// TypeScript or JavaScript project-graph context.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TypeScriptProjectContext {
    config_digest: String,
    package_lock_digest: Option<String>,
    project_references: Vec<String>,
    source_roots: Vec<String>,
    type_roots: Vec<String>,
    path_mappings: Vec<ProjectContextEvidence>,
    module_resolution: String,
    language_target: String,
    jsx_mode: Option<String>,
    semantic_frontend_version: Option<String>,
    precise_index_digest: Option<String>,
}

impl TypeScriptProjectContext {
    /// Returns the normalized `tsconfig` or `jsconfig` digest.
    #[must_use]
    pub fn config_digest(&self) -> &str {
        &self.config_digest
    }

    /// Returns the package-lock digest when supplied.
    #[must_use]
    pub fn package_lock_digest(&self) -> Option<&str> {
        self.package_lock_digest.as_deref()
    }

    /// Returns canonical project references.
    #[must_use]
    pub fn project_references(&self) -> &[String] {
        &self.project_references
    }

    /// Returns project-relative source roots.
    #[must_use]
    pub fn source_roots(&self) -> &[String] {
        &self.source_roots
    }

    /// Returns project-relative type roots.
    #[must_use]
    pub fn type_roots(&self) -> &[String] {
        &self.type_roots
    }

    /// Returns normalized path aliases with their declarative sources.
    #[must_use]
    pub fn path_mappings(&self) -> &[ProjectContextEvidence] {
        &self.path_mappings
    }

    /// Returns the selected module-resolution mode.
    #[must_use]
    pub fn module_resolution(&self) -> &str {
        &self.module_resolution
    }

    /// Returns the selected JavaScript language target.
    #[must_use]
    pub fn language_target(&self) -> &str {
        &self.language_target
    }

    /// Returns the selected JSX mode when applicable.
    #[must_use]
    pub fn jsx_mode(&self) -> Option<&str> {
        self.jsx_mode.as_deref()
    }

    /// Returns the semantic frontend version when captured.
    #[must_use]
    pub fn semantic_frontend_version(&self) -> Option<&str> {
        self.semantic_frontend_version.as_deref()
    }

    /// Returns a compiler-precise index digest when captured.
    #[must_use]
    pub fn precise_index_digest(&self) -> Option<&str> {
        self.precise_index_digest.as_deref()
    }
}

/// Python package and type-environment context.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PythonProjectContext {
    pyproject_digest: Option<String>,
    lock_digest: Option<String>,
    package_roots: Vec<String>,
    import_paths: Vec<String>,
    namespace_packages: Vec<String>,
    stub_roots: Vec<String>,
    type_checker: Option<String>,
    semantic_frontend_version: Option<String>,
    precise_index_digest: Option<String>,
}

impl PythonProjectContext {
    /// Returns the `pyproject.toml` digest when supplied.
    #[must_use]
    pub fn pyproject_digest(&self) -> Option<&str> {
        self.pyproject_digest.as_deref()
    }

    /// Returns the dependency lock digest when supplied.
    #[must_use]
    pub fn lock_digest(&self) -> Option<&str> {
        self.lock_digest.as_deref()
    }

    /// Returns project-relative package roots.
    #[must_use]
    pub fn package_roots(&self) -> &[String] {
        &self.package_roots
    }

    /// Returns project-relative import search paths.
    #[must_use]
    pub fn import_paths(&self) -> &[String] {
        &self.import_paths
    }

    /// Returns declared namespace packages.
    #[must_use]
    pub fn namespace_packages(&self) -> &[String] {
        &self.namespace_packages
    }

    /// Returns project-relative stub roots.
    #[must_use]
    pub fn stub_roots(&self) -> &[String] {
        &self.stub_roots
    }

    /// Returns the selected type-checker identity.
    #[must_use]
    pub fn type_checker(&self) -> Option<&str> {
        self.type_checker.as_deref()
    }

    /// Returns the semantic frontend version when captured.
    #[must_use]
    pub fn semantic_frontend_version(&self) -> Option<&str> {
        self.semantic_frontend_version.as_deref()
    }

    /// Returns a compiler-precise index digest when captured.
    #[must_use]
    pub fn precise_index_digest(&self) -> Option<&str> {
        self.precise_index_digest.as_deref()
    }
}

/// Go module, workspace, and build-selection context.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GoProjectContext {
    go_mod_digest: String,
    go_work_digest: Option<String>,
    go_sum_digest: Option<String>,
    modules: Vec<String>,
    packages: Vec<String>,
    replacements: Vec<ProjectContextEvidence>,
    build_tags: Vec<String>,
    goos: String,
    goarch: String,
    vendor_mode: bool,
    cgo_enabled: bool,
    semantic_frontend_version: Option<String>,
    precise_index_digest: Option<String>,
}

impl GoProjectContext {
    /// Returns the primary `go.mod` digest.
    #[must_use]
    pub fn go_mod_digest(&self) -> &str {
        &self.go_mod_digest
    }

    /// Returns the `go.work` digest when supplied.
    #[must_use]
    pub fn go_work_digest(&self) -> Option<&str> {
        self.go_work_digest.as_deref()
    }

    /// Returns the `go.sum` digest when supplied.
    #[must_use]
    pub fn go_sum_digest(&self) -> Option<&str> {
        self.go_sum_digest.as_deref()
    }

    /// Returns canonical module identities.
    #[must_use]
    pub fn modules(&self) -> &[String] {
        &self.modules
    }

    /// Returns canonical package import paths.
    #[must_use]
    pub fn packages(&self) -> &[String] {
        &self.packages
    }

    /// Returns normalized module replacements with their sources.
    #[must_use]
    pub fn replacements(&self) -> &[ProjectContextEvidence] {
        &self.replacements
    }

    /// Returns active Go build tags.
    #[must_use]
    pub fn build_tags(&self) -> &[String] {
        &self.build_tags
    }

    /// Returns the selected `GOOS`.
    #[must_use]
    pub fn goos(&self) -> &str {
        &self.goos
    }

    /// Returns the selected `GOARCH`.
    #[must_use]
    pub fn goarch(&self) -> &str {
        &self.goarch
    }

    /// Returns whether vendor mode is selected.
    #[must_use]
    pub const fn vendor_mode(&self) -> bool {
        self.vendor_mode
    }

    /// Returns whether cgo participation is selected.
    #[must_use]
    pub const fn cgo_enabled(&self) -> bool {
        self.cgo_enabled
    }

    /// Returns the semantic frontend version when captured.
    #[must_use]
    pub fn semantic_frontend_version(&self) -> Option<&str> {
        self.semantic_frontend_version.as_deref()
    }

    /// Returns a compiler-precise index digest when captured.
    #[must_use]
    pub fn precise_index_digest(&self) -> Option<&str> {
        self.precise_index_digest.as_deref()
    }
}

/// C or C++ compile-database context.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CxxProjectContext {
    compile_database_digest: String,
    working_directory: String,
    include_paths: Vec<String>,
    defines: Vec<String>,
    conditional_symbols: Vec<String>,
    macros: Vec<ProjectContextEvidence>,
}

impl CxxProjectContext {
    /// Returns the lowercase hexadecimal digest of the compile database.
    #[must_use]
    pub fn compile_database_digest(&self) -> &str {
        &self.compile_database_digest
    }

    /// Returns the captured compiler working directory.
    #[must_use]
    pub fn working_directory(&self) -> &str {
        &self.working_directory
    }

    /// Returns normalized include search paths.
    #[must_use]
    pub fn include_paths(&self) -> &[String] {
        &self.include_paths
    }

    /// Returns normalized preprocessor definitions.
    #[must_use]
    pub fn defines(&self) -> &[String] {
        &self.defines
    }

    /// Returns symbols controlling conditional compilation.
    #[must_use]
    pub fn conditional_symbols(&self) -> &[String] {
        &self.conditional_symbols
    }

    /// Returns macro values with their declarative origins.
    #[must_use]
    pub fn macros(&self) -> &[ProjectContextEvidence] {
        &self.macros
    }
}

/// Java or Kotlin declarative project-model context.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JvmProjectContext {
    project_model_digest: String,
    targets: Vec<String>,
    source_sets: Vec<String>,
    classpath_entries: Vec<String>,
    generated_roots: Vec<String>,
    framework_routes: Vec<ProjectContextEvidence>,
}

impl JvmProjectContext {
    /// Returns the lowercase hexadecimal digest of the project model.
    #[must_use]
    pub fn project_model_digest(&self) -> &str {
        &self.project_model_digest
    }

    /// Returns normalized build targets.
    #[must_use]
    pub fn targets(&self) -> &[String] {
        &self.targets
    }

    /// Returns normalized source-set identities.
    #[must_use]
    pub fn source_sets(&self) -> &[String] {
        &self.source_sets
    }

    /// Returns normalized classpath entries.
    #[must_use]
    pub fn classpath_entries(&self) -> &[String] {
        &self.classpath_entries
    }

    /// Returns roots containing generated sources.
    #[must_use]
    pub fn generated_roots(&self) -> &[String] {
        &self.generated_roots
    }

    /// Returns framework routes with their declarative origins.
    #[must_use]
    pub fn framework_routes(&self) -> &[ProjectContextEvidence] {
        &self.framework_routes
    }
}

/// C# declarative solution and project context.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DotnetProjectContext {
    project_model_digest: String,
    projects: Vec<String>,
    target_frameworks: Vec<String>,
    partial_types: Vec<ProjectContextEvidence>,
    async_symbols: Vec<ProjectContextEvidence>,
    delegates: Vec<ProjectContextEvidence>,
    linq_expressions: Vec<ProjectContextEvidence>,
    routes: Vec<ProjectContextEvidence>,
}

impl DotnetProjectContext {
    /// Returns the lowercase hexadecimal digest of the solution model.
    #[must_use]
    pub fn project_model_digest(&self) -> &str {
        &self.project_model_digest
    }

    /// Returns normalized project identities.
    #[must_use]
    pub fn projects(&self) -> &[String] {
        &self.projects
    }

    /// Returns normalized target framework monikers.
    #[must_use]
    pub fn target_frameworks(&self) -> &[String] {
        &self.target_frameworks
    }

    /// Returns partial-type declarations with their origins.
    #[must_use]
    pub fn partial_types(&self) -> &[ProjectContextEvidence] {
        &self.partial_types
    }

    /// Returns async symbols with their origins.
    #[must_use]
    pub fn async_symbols(&self) -> &[ProjectContextEvidence] {
        &self.async_symbols
    }

    /// Returns delegate bindings with their origins.
    #[must_use]
    pub fn delegates(&self) -> &[ProjectContextEvidence] {
        &self.delegates
    }

    /// Returns LINQ expressions with their origins.
    #[must_use]
    pub fn linq_expressions(&self) -> &[ProjectContextEvidence] {
        &self.linq_expressions
    }

    /// Returns framework routes with their declarative origins.
    #[must_use]
    pub fn routes(&self) -> &[ProjectContextEvidence] {
        &self.routes
    }
}

/// PHP Composer and dynamic-resolution context.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PhpProjectContext {
    composer_lock_digest: String,
    autoload_roots: Vec<String>,
    namespaces: Vec<String>,
    traits: Vec<ProjectContextEvidence>,
    dynamic_calls: Vec<ProjectContextEvidence>,
    routes: Vec<ProjectContextEvidence>,
}

impl PhpProjectContext {
    /// Returns the lowercase hexadecimal digest of the Composer lock data.
    #[must_use]
    pub fn composer_lock_digest(&self) -> &str {
        &self.composer_lock_digest
    }

    /// Returns normalized autoload roots.
    #[must_use]
    pub fn autoload_roots(&self) -> &[String] {
        &self.autoload_roots
    }

    /// Returns normalized namespace prefixes.
    #[must_use]
    pub fn namespaces(&self) -> &[String] {
        &self.namespaces
    }

    /// Returns trait uses with their declarative origins.
    #[must_use]
    pub fn traits(&self) -> &[ProjectContextEvidence] {
        &self.traits
    }

    /// Returns dynamic call candidates with their declarative origins.
    #[must_use]
    pub fn dynamic_calls(&self) -> &[ProjectContextEvidence] {
        &self.dynamic_calls
    }

    /// Returns framework routes with their declarative origins.
    #[must_use]
    pub fn routes(&self) -> &[ProjectContextEvidence] {
        &self.routes
    }
}

/// Language-specific metadata carried by a project context.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum ProjectContextMetadata {
    /// Rust metadata captured from Cargo and an optional semantic frontend.
    Rust(RustProjectContext),
    /// TypeScript or JavaScript metadata captured from project configuration.
    Typescript(TypeScriptProjectContext),
    /// Python metadata captured from package and type-checker configuration.
    Python(PythonProjectContext),
    /// Go metadata captured from module and build-selection configuration.
    Go(GoProjectContext),
    /// C or C++ metadata captured from a compile database.
    Cxx(CxxProjectContext),
    /// Java or Kotlin metadata captured from a declarative project model.
    Jvm(JvmProjectContext),
    /// C# metadata captured from solution and project files.
    Dotnet(DotnetProjectContext),
    /// PHP metadata captured from Composer and framework configuration.
    Php(PhpProjectContext),
}

/// Validated and canonicalized declarative project context.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProjectContext {
    schema_version: u16,
    language: ProjectContextLanguage,
    target: String,
    files: Vec<ProjectContextFile>,
    metadata: ProjectContextMetadata,
    coverage: ProjectContextCoverage,
    #[serde(skip)]
    identity: BuildContextIdentity,
}

impl ProjectContext {
    /// Decodes, validates, and canonicalizes one project-context manifest.
    ///
    /// # Errors
    ///
    /// Returns [`ProjectContextError`] when the input is oversized, malformed,
    /// incompatible, internally inconsistent, or exceeds a collection limit.
    pub fn decode_json(input: &[u8]) -> Result<Self, ProjectContextError> {
        if input.is_empty() {
            return Err(ProjectContextError::EmptyInput);
        }
        if input.len() > MAX_PROJECT_CONTEXT_BYTES {
            return Err(ProjectContextError::InputTooLarge {
                observed: input.len(),
                limit: MAX_PROJECT_CONTEXT_BYTES,
            });
        }
        let mut context: Self = serde_json::from_slice::<WireProjectContext>(input)?.into_context();
        context.validate_and_normalize()?;
        context.identity = context.derive_identity()?;
        Ok(context)
    }

    /// Returns the accepted schema version.
    #[must_use]
    pub const fn schema_version(&self) -> u16 {
        self.schema_version
    }

    /// Returns the language interpreted by this build context.
    #[must_use]
    pub const fn language(&self) -> ProjectContextLanguage {
        self.language
    }

    /// Returns the declarative target identity.
    #[must_use]
    pub fn target(&self) -> &str {
        &self.target
    }

    /// Returns source files in canonical path order.
    #[must_use]
    pub fn files(&self) -> &[ProjectContextFile] {
        &self.files
    }

    /// Returns language-specific project metadata.
    #[must_use]
    pub const fn metadata(&self) -> &ProjectContextMetadata {
        &self.metadata
    }

    /// Returns the declared metadata coverage.
    #[must_use]
    pub const fn coverage(&self) -> &ProjectContextCoverage {
        &self.coverage
    }

    /// Returns the deterministic identity of the normalized context.
    #[must_use]
    pub const fn identity(&self) -> BuildContextIdentity {
        self.identity
    }

    fn validate_and_normalize(&mut self) -> Result<(), ProjectContextError> {
        if self.schema_version != PROJECT_CONTEXT_SCHEMA_VERSION {
            return Err(ProjectContextError::UnsupportedSchemaVersion {
                observed: self.schema_version,
                supported: PROJECT_CONTEXT_SCHEMA_VERSION,
            });
        }
        let mut validator = Validator::default();
        validator.string(&self.target)?;
        if self.files.is_empty() {
            return Err(ProjectContextError::MissingFiles);
        }
        validator.items(self.files.len())?;
        for file in &self.files {
            validator.string(&file.path)?;
            if !is_normalized_relative_path(&file.path) {
                return Err(ProjectContextError::InvalidFilePath);
            }
            if let Some(origin) = &file.generated_from {
                validator.string(origin)?;
                if !is_normalized_relative_path(origin) {
                    return Err(ProjectContextError::InvalidFilePath);
                }
            }
            validate_origin_mappings(&file.origin_mappings, &mut validator)?;
        }
        self.files.sort();
        reject_duplicates(&self.files)?;
        if self
            .files
            .windows(2)
            .any(|pair| pair[0].path == pair[1].path)
        {
            return Err(ProjectContextError::DuplicateValue);
        }
        self.coverage.validate_and_normalize(&mut validator)?;
        self.metadata
            .validate_and_normalize(self.language, &mut validator)
    }

    fn derive_identity(&self) -> Result<BuildContextIdentity, ProjectContextError> {
        let canonical =
            serde_json::to_vec(self).map_err(|_| ProjectContextError::IdentityEncoding)?;
        let capacity = IDENTITY_DOMAIN
            .len()
            .checked_add(canonical.len())
            .ok_or(ProjectContextError::IdentityEncoding)?;
        let mut material = Vec::with_capacity(capacity);
        material.extend_from_slice(IDENTITY_DOMAIN);
        material.extend_from_slice(&canonical);
        Ok(BuildContextIdentity::new(content_hash(&material)))
    }
}

impl ProjectContextCoverage {
    fn validate_and_normalize(
        &mut self,
        validator: &mut Validator,
    ) -> Result<(), ProjectContextError> {
        validator.items(self.skips.len())?;
        for skip in &self.skips {
            validator.code(&skip.code)?;
            validator.string(&skip.detail)?;
        }
        self.skips.sort();
        reject_duplicates(&self.skips)?;
        if self.status == ProjectContextCoverageStatus::Complete && !self.skips.is_empty() {
            return Err(ProjectContextError::ContradictoryCoverage);
        }
        if self.status != ProjectContextCoverageStatus::Complete && self.skips.is_empty() {
            return Err(ProjectContextError::MissingSkipReason);
        }
        Ok(())
    }
}

impl ProjectContextMetadata {
    fn validate_and_normalize(
        &mut self,
        language: ProjectContextLanguage,
        validator: &mut Validator,
    ) -> Result<(), ProjectContextError> {
        match (language, self) {
            (ProjectContextLanguage::Rust, Self::Rust(context)) => {
                context.validate_and_normalize(validator)
            }
            (
                ProjectContextLanguage::Typescript | ProjectContextLanguage::Javascript,
                Self::Typescript(context),
            ) => context.validate_and_normalize(validator),
            (ProjectContextLanguage::Python, Self::Python(context)) => {
                context.validate_and_normalize(validator)
            }
            (ProjectContextLanguage::Go, Self::Go(context)) => {
                context.validate_and_normalize(validator)
            }
            (ProjectContextLanguage::C | ProjectContextLanguage::Cpp, Self::Cxx(context)) => {
                context.validate_and_normalize(validator)
            }
            (ProjectContextLanguage::Java | ProjectContextLanguage::Kotlin, Self::Jvm(context)) => {
                context.validate_and_normalize(validator)
            }
            (ProjectContextLanguage::Csharp, Self::Dotnet(context)) => {
                context.validate_and_normalize(validator)
            }
            (ProjectContextLanguage::Php, Self::Php(context)) => {
                context.validate_and_normalize(validator)
            }
            _ => Err(ProjectContextError::LanguageMetadataMismatch),
        }
    }
}

impl RustProjectContext {
    fn validate_and_normalize(
        &mut self,
        validator: &mut Validator,
    ) -> Result<(), ProjectContextError> {
        validator.digest(&self.cargo_metadata_digest)?;
        validator.optional_digest(self.cargo_lock_digest.as_deref())?;
        if self.workspace_members.is_empty()
            || self.targets.is_empty()
            || self.source_roots.is_empty()
        {
            return Err(ProjectContextError::MissingProjectStructure);
        }
        normalize_strings(&mut self.workspace_members, validator)?;
        normalize_strings(&mut self.targets, validator)?;
        normalize_paths(&mut self.source_roots, validator)?;
        validator.string(&self.edition)?;
        if !matches!(self.edition.as_str(), "2015" | "2018" | "2021" | "2024") {
            return Err(ProjectContextError::InvalidLanguageOption);
        }
        validator.string(&self.target_triple)?;
        normalize_strings(&mut self.enabled_features, validator)?;
        normalize_strings(&mut self.cfgs, validator)?;
        validator.optional_string(self.compiler_version.as_deref())?;
        validator.optional_digest(self.precise_index_digest.as_deref())
    }
}

impl TypeScriptProjectContext {
    fn validate_and_normalize(
        &mut self,
        validator: &mut Validator,
    ) -> Result<(), ProjectContextError> {
        validator.digest(&self.config_digest)?;
        validator.optional_digest(self.package_lock_digest.as_deref())?;
        if self.source_roots.is_empty() {
            return Err(ProjectContextError::MissingProjectStructure);
        }
        normalize_paths(&mut self.project_references, validator)?;
        normalize_paths(&mut self.source_roots, validator)?;
        normalize_paths(&mut self.type_roots, validator)?;
        normalize_evidence(&mut self.path_mappings, validator)?;
        validator.string(&self.module_resolution)?;
        validator.string(&self.language_target)?;
        validator.optional_string(self.jsx_mode.as_deref())?;
        validator.optional_string(self.semantic_frontend_version.as_deref())?;
        validator.optional_digest(self.precise_index_digest.as_deref())
    }
}

impl PythonProjectContext {
    fn validate_and_normalize(
        &mut self,
        validator: &mut Validator,
    ) -> Result<(), ProjectContextError> {
        validator.optional_digest(self.pyproject_digest.as_deref())?;
        validator.optional_digest(self.lock_digest.as_deref())?;
        if self.package_roots.is_empty() {
            return Err(ProjectContextError::MissingProjectStructure);
        }
        normalize_paths(&mut self.package_roots, validator)?;
        normalize_paths(&mut self.import_paths, validator)?;
        normalize_strings(&mut self.namespace_packages, validator)?;
        normalize_paths(&mut self.stub_roots, validator)?;
        validator.optional_string(self.type_checker.as_deref())?;
        validator.optional_string(self.semantic_frontend_version.as_deref())?;
        validator.optional_digest(self.precise_index_digest.as_deref())
    }
}

impl GoProjectContext {
    fn validate_and_normalize(
        &mut self,
        validator: &mut Validator,
    ) -> Result<(), ProjectContextError> {
        validator.digest(&self.go_mod_digest)?;
        validator.optional_digest(self.go_work_digest.as_deref())?;
        validator.optional_digest(self.go_sum_digest.as_deref())?;
        if self.modules.is_empty() || self.packages.is_empty() {
            return Err(ProjectContextError::MissingProjectStructure);
        }
        normalize_strings(&mut self.modules, validator)?;
        normalize_strings(&mut self.packages, validator)?;
        normalize_evidence(&mut self.replacements, validator)?;
        normalize_strings(&mut self.build_tags, validator)?;
        validator.string(&self.goos)?;
        validator.string(&self.goarch)?;
        validator.optional_string(self.semantic_frontend_version.as_deref())?;
        validator.optional_digest(self.precise_index_digest.as_deref())
    }
}

impl CxxProjectContext {
    fn validate_and_normalize(
        &mut self,
        validator: &mut Validator,
    ) -> Result<(), ProjectContextError> {
        validator.digest(&self.compile_database_digest)?;
        validator.string(&self.working_directory)?;
        normalize_strings(&mut self.include_paths, validator)?;
        normalize_strings(&mut self.defines, validator)?;
        normalize_strings(&mut self.conditional_symbols, validator)?;
        normalize_evidence(&mut self.macros, validator)
    }
}

impl JvmProjectContext {
    fn validate_and_normalize(
        &mut self,
        validator: &mut Validator,
    ) -> Result<(), ProjectContextError> {
        validator.digest(&self.project_model_digest)?;
        if self.targets.is_empty() {
            return Err(ProjectContextError::MissingTargets);
        }
        normalize_strings(&mut self.targets, validator)?;
        normalize_strings(&mut self.source_sets, validator)?;
        normalize_strings(&mut self.classpath_entries, validator)?;
        normalize_strings(&mut self.generated_roots, validator)?;
        normalize_evidence(&mut self.framework_routes, validator)
    }
}

impl DotnetProjectContext {
    fn validate_and_normalize(
        &mut self,
        validator: &mut Validator,
    ) -> Result<(), ProjectContextError> {
        validator.digest(&self.project_model_digest)?;
        if self.projects.is_empty() {
            return Err(ProjectContextError::MissingProjects);
        }
        normalize_strings(&mut self.projects, validator)?;
        normalize_strings(&mut self.target_frameworks, validator)?;
        normalize_evidence(&mut self.partial_types, validator)?;
        normalize_evidence(&mut self.async_symbols, validator)?;
        normalize_evidence(&mut self.delegates, validator)?;
        normalize_evidence(&mut self.linq_expressions, validator)?;
        normalize_evidence(&mut self.routes, validator)
    }
}

impl PhpProjectContext {
    fn validate_and_normalize(
        &mut self,
        validator: &mut Validator,
    ) -> Result<(), ProjectContextError> {
        validator.digest(&self.composer_lock_digest)?;
        if self.autoload_roots.is_empty() {
            return Err(ProjectContextError::MissingAutoloadRoots);
        }
        normalize_strings(&mut self.autoload_roots, validator)?;
        normalize_strings(&mut self.namespaces, validator)?;
        normalize_evidence(&mut self.traits, validator)?;
        normalize_evidence(&mut self.dynamic_calls, validator)?;
        normalize_evidence(&mut self.routes, validator)
    }
}

fn validate_origin_mappings(
    mappings: &[GeneratedOriginMapping],
    validator: &mut Validator,
) -> Result<(), ProjectContextError> {
    validator.items(mappings.len())?;
    let mut previous_end = 0_u64;
    for (index, mapping) in mappings.iter().enumerate() {
        validator.string(&mapping.origin_path)?;
        validator.string(&mapping.transformation)?;
        validator.optional_digest(mapping.generator_digest.as_deref())?;
        if !is_normalized_relative_path(&mapping.origin_path)
            || mapping.generated_start_byte >= mapping.generated_end_byte
            || mapping.origin_start_byte >= mapping.origin_end_byte
            || (index != 0 && mapping.generated_start_byte < previous_end)
        {
            return Err(ProjectContextError::InvalidOriginMapping { index });
        }
        previous_end = mapping.generated_end_byte;
    }
    Ok(())
}

fn normalize_strings(
    values: &mut Vec<String>,
    validator: &mut Validator,
) -> Result<(), ProjectContextError> {
    validator.items(values.len())?;
    for value in &*values {
        validator.string(value)?;
    }
    values.sort();
    reject_duplicates(values)
}

fn normalize_paths(
    values: &mut Vec<String>,
    validator: &mut Validator,
) -> Result<(), ProjectContextError> {
    validator.items(values.len())?;
    for value in &*values {
        validator.string(value)?;
        if !is_normalized_relative_path(value) {
            return Err(ProjectContextError::InvalidFilePath);
        }
    }
    values.sort();
    reject_duplicates(values)
}

fn normalize_evidence(
    values: &mut Vec<ProjectContextEvidence>,
    validator: &mut Validator,
) -> Result<(), ProjectContextError> {
    validator.items(values.len())?;
    for evidence in &*values {
        validator.string(&evidence.value)?;
        validator.string(&evidence.source)?;
    }
    values.sort();
    reject_duplicates(values)
}

fn reject_duplicates<T: PartialEq>(values: &[T]) -> Result<(), ProjectContextError> {
    if values.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err(ProjectContextError::DuplicateValue);
    }
    Ok(())
}

fn is_normalized_relative_path(value: &str) -> bool {
    !value.starts_with('/')
        && !value.starts_with('\\')
        && !value.contains('\\')
        && !value.split('/').any(|component| {
            component.is_empty() || component == "." || component == ".." || component.contains(':')
        })
}

#[derive(Default)]
struct Validator {
    items: usize,
    string_bytes: usize,
}

impl Validator {
    fn items(&mut self, count: usize) -> Result<(), ProjectContextError> {
        self.items = self
            .items
            .checked_add(count)
            .ok_or(ProjectContextError::TooManyItems)?;
        if self.items > MAX_PROJECT_CONTEXT_ITEMS {
            return Err(ProjectContextError::TooManyItems);
        }
        Ok(())
    }

    fn string(&mut self, value: &str) -> Result<(), ProjectContextError> {
        if value.is_empty() || value.len() > MAX_PROJECT_CONTEXT_STRING_BYTES {
            return Err(ProjectContextError::InvalidString);
        }
        self.items(1)?;
        self.string_bytes = self
            .string_bytes
            .checked_add(value.len())
            .ok_or(ProjectContextError::TooManyStringBytes)?;
        if self.string_bytes > MAX_PROJECT_CONTEXT_BYTES {
            return Err(ProjectContextError::TooManyStringBytes);
        }
        Ok(())
    }

    fn optional_string(&mut self, value: Option<&str>) -> Result<(), ProjectContextError> {
        value.map_or(Ok(()), |value| self.string(value))
    }

    fn code(&mut self, value: &str) -> Result<(), ProjectContextError> {
        self.string(value)?;
        let bytes = value.as_bytes();
        if !bytes[0].is_ascii_lowercase()
            || bytes.last() == Some(&b'_')
            || bytes.windows(2).any(|pair| pair == b"__")
            || bytes
                .iter()
                .any(|byte| !byte.is_ascii_lowercase() && !byte.is_ascii_digit() && *byte != b'_')
        {
            return Err(ProjectContextError::InvalidSkipCode);
        }
        Ok(())
    }

    fn digest(&mut self, value: &str) -> Result<(), ProjectContextError> {
        self.string(value)?;
        if value.len() != 64
            || value
                .as_bytes()
                .iter()
                .any(|byte| !byte.is_ascii_hexdigit() || byte.is_ascii_uppercase())
        {
            return Err(ProjectContextError::InvalidDigest);
        }
        Ok(())
    }

    fn optional_digest(&mut self, value: Option<&str>) -> Result<(), ProjectContextError> {
        value.map_or(Ok(()), |value| self.digest(value))
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireProjectContext {
    schema_version: u16,
    language: ProjectContextLanguage,
    target: String,
    files: Vec<ProjectContextFile>,
    metadata: ProjectContextMetadata,
    coverage: ProjectContextCoverage,
}

impl WireProjectContext {
    fn into_context(self) -> ProjectContext {
        ProjectContext {
            schema_version: self.schema_version,
            language: self.language,
            target: self.target,
            files: self.files,
            metadata: self.metadata,
            coverage: self.coverage,
            identity: BuildContextIdentity::new(content_hash(&[])),
        }
    }
}

/// Invalid or unsupported declarative project context.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ProjectContextError {
    /// The input did not contain a manifest.
    #[error("project context input must not be empty")]
    EmptyInput,
    /// The encoded manifest exceeded the hard byte ceiling.
    #[error("project context input is {observed} bytes, limit is {limit}")]
    InputTooLarge {
        /// Encoded byte length.
        observed: usize,
        /// Hard input ceiling.
        limit: usize,
    },
    /// The JSON document did not match the strict schema.
    #[error("project context JSON is invalid: {0}")]
    Json(#[from] serde_json::Error),
    /// The schema version is not supported by this build.
    #[error("project context schema version {observed} is unsupported; expected {supported}")]
    UnsupportedSchemaVersion {
        /// Version declared by the input.
        observed: u16,
        /// Version supported by this build.
        supported: u16,
    },
    /// A required string was empty or exceeded its hard ceiling.
    #[error("project context contains an invalid string")]
    InvalidString,
    /// Aggregated string bytes exceeded the hard context ceiling.
    #[error("project context contains too many string bytes")]
    TooManyStringBytes,
    /// Aggregated repeated values exceeded the hard item ceiling.
    #[error("project context contains too many items")]
    TooManyItems,
    /// A digest was not 64 lowercase hexadecimal characters.
    #[error("project context contains an invalid digest")]
    InvalidDigest,
    /// A source path was not normalized and project-relative.
    #[error("project context contains an invalid source path")]
    InvalidFilePath,
    /// A generated-to-origin range was empty, unordered, or named an unsafe path.
    #[error("project context contains invalid generated-origin mapping {index}")]
    InvalidOriginMapping {
        /// Zero-based mapping index within its generated file.
        index: usize,
    },
    /// A stable skip code was not lowercase `snake_case`.
    #[error("project context contains an invalid skip code")]
    InvalidSkipCode,
    /// A canonical collection contained the same value more than once.
    #[error("project context contains a duplicate value")]
    DuplicateValue,
    /// The language and metadata family do not agree.
    #[error("project context language does not match its metadata family")]
    LanguageMetadataMismatch,
    /// The context did not name any source files.
    #[error("project context must contain at least one source file")]
    MissingFiles,
    /// A JVM project model did not name any build target.
    #[error("JVM project context must contain at least one target")]
    MissingTargets,
    /// A .NET project model did not name any project.
    #[error(".NET project context must contain at least one project")]
    MissingProjects,
    /// A PHP project model did not name any Composer autoload root.
    #[error("PHP project context must contain at least one autoload root")]
    MissingAutoloadRoots,
    /// A target-language context omitted its required project roots or targets.
    #[error("project context omits required project structure")]
    MissingProjectStructure,
    /// A language-specific option was outside the supported normalized set.
    #[error("project context contains an invalid language option")]
    InvalidLanguageOption,
    /// Complete coverage was paired with one or more skip reasons.
    #[error("complete project context coverage cannot contain skip reasons")]
    ContradictoryCoverage,
    /// Incomplete coverage omitted the reason for lowering confidence.
    #[error("incomplete project context coverage requires a skip reason")]
    MissingSkipReason,
    /// Canonical identity material could not be encoded.
    #[error("project context identity could not be encoded")]
    IdentityEncoding,
}
