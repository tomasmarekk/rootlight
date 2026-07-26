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

/// One project-relative source file and its optional generated origin.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectContextFile {
    path: String,
    generated_from: Option<String>,
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
            }
        }
        self.files.sort();
        reject_duplicates(&self.files)?;
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
