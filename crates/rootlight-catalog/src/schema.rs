//! Fixed SQLite DDL, defensive settings, and integrity verification.
//!
//! SQL never crosses the crate boundary. The generated compatibility fixture
//! pins only application IDs, versions, and checksums of this private ledger.

use std::{
    collections::BTreeMap,
    fs::{self, OpenOptions},
    path::Path,
    time::Duration,
};

use rootlight_ids::content_hash;
use rootlight_storage::GenerationContext;
use rusqlite::{
    Connection, OpenFlags, OptionalExtension,
    config::DbConfig,
    hooks::{AuthAction, Authorization},
    limits::Limit,
    params,
};

use crate::{
    CatalogError, CatalogErrorKind, MIN_SQLITE_VERSION_NUMBER, SQLITE_BUSY_TIMEOUT_MS,
    SchemaCompatibility,
};

const CONTROL_APPLICATION_ID: u32 = 0x524c_4354;
const ORACLE_APPLICATION_ID: u32 = 0x524c_4f52;
const CONTROL_SCHEMA_VERSION: u32 = 2;
const ORACLE_SCHEMA_VERSION: u32 = 2;

const APPLICATION_META_SQL: &str = "CREATE TABLE application_meta (
    key TEXT PRIMARY KEY NOT NULL,
    value BLOB NOT NULL CHECK(length(value) BETWEEN 1 AND 128)
) STRICT";
const MIGRATIONS_SQL: &str = "CREATE TABLE migrations (
    migration_id INTEGER PRIMARY KEY NOT NULL,
    checksum BLOB NOT NULL CHECK(length(checksum) = 32)
) STRICT";

const GENERATION_META_SQL: &str = "CREATE TABLE generation_meta (
    singleton INTEGER PRIMARY KEY NOT NULL CHECK(singleton = 1),
    contract_major INTEGER NOT NULL CHECK(contract_major >= 1 AND contract_major <= 65535),
    contract_minor INTEGER NOT NULL CHECK(contract_minor >= 0 AND contract_minor <= 65535),
    ir_major INTEGER NOT NULL CHECK(ir_major >= 1 AND ir_major <= 65535),
    ir_minor INTEGER NOT NULL CHECK(ir_minor >= 0 AND ir_minor <= 65535),
    repository_id BLOB NOT NULL CHECK(length(repository_id) = 16),
    generation_id BLOB NOT NULL CHECK(length(generation_id) = 20),
    parent_generation_id BLOB CHECK(parent_generation_id IS NULL OR length(parent_generation_id) = 20),
    manifest_hash BLOB NOT NULL CHECK(length(manifest_hash) = 32),
    configuration_hash BLOB NOT NULL CHECK(length(configuration_hash) = 32),
    provider_set_hash BLOB NOT NULL CHECK(length(provider_set_hash) = 32),
    file_count INTEGER NOT NULL CHECK(file_count >= 0),
    entity_count INTEGER NOT NULL CHECK(entity_count >= 0),
    occurrence_count INTEGER NOT NULL CHECK(occurrence_count >= 0),
    relation_count INTEGER NOT NULL CHECK(relation_count >= 0),
    provenance_count INTEGER NOT NULL CHECK(provenance_count >= 0),
    source_mapping_count INTEGER NOT NULL CHECK(source_mapping_count >= 0),
    coverage_count INTEGER NOT NULL CHECK(coverage_count >= 0),
    skipped_region_count INTEGER NOT NULL CHECK(skipped_region_count >= 0),
    diagnostic_count INTEGER NOT NULL CHECK(diagnostic_count >= 0),
    extension_count INTEGER NOT NULL CHECK(extension_count >= 0),
    source_ref_count INTEGER NOT NULL CHECK(source_ref_count >= 0),
    stored_row_count INTEGER NOT NULL CHECK(stored_row_count >= 1),
    text_bytes INTEGER NOT NULL CHECK(text_bytes >= 0),
    sealed INTEGER NOT NULL CHECK(sealed = 1),
    UNIQUE(repository_id, generation_id),
    CHECK(parent_generation_id IS NULL OR parent_generation_id != generation_id)
) STRICT";

const IDENTITY_REGISTRY_SQL: &str = "CREATE TABLE identity_registry (
    kind TEXT NOT NULL CHECK(kind IN ('repository', 'file', 'entity', 'fact')),
    identity BLOB NOT NULL,
    PRIMARY KEY(kind, identity),
    CHECK((kind = 'repository' AND length(identity) = 16)
       OR (kind IN ('file', 'entity', 'fact') AND length(identity) = 20))
) STRICT";

const SOURCE_REFS_SQL: &str = "CREATE TABLE source_refs (
    ordinal INTEGER PRIMARY KEY NOT NULL CHECK(ordinal >= 0),
    repository_id BLOB NOT NULL CHECK(length(repository_id) = 16),
    generation_id BLOB NOT NULL CHECK(length(generation_id) = 20),
    file_id BLOB NOT NULL CHECK(length(file_id) = 20),
    start_byte INTEGER NOT NULL CHECK(start_byte >= 0),
    end_byte INTEGER NOT NULL CHECK(end_byte >= start_byte),
    content_hash BLOB NOT NULL CHECK(length(content_hash) = 32),
    line_start INTEGER NOT NULL CHECK(line_start >= 0),
    line_end INTEGER NOT NULL CHECK(line_end >= 0),
    UNIQUE(repository_id, generation_id, file_id, start_byte, end_byte, content_hash, line_start, line_end),
    CHECK((line_start = 0 AND line_end = 0)
       OR (line_start >= 1 AND line_end >= line_start)),
    FOREIGN KEY(repository_id, generation_id)
        REFERENCES generation_meta(repository_id, generation_id)
        DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY(file_id) REFERENCES files(file_id)
        DEFERRABLE INITIALLY DEFERRED
) STRICT";

const PROVENANCE_SQL: &str = "CREATE TABLE provenance (
    provenance_id BLOB PRIMARY KEY NOT NULL CHECK(length(provenance_id) = 20),
    repository_id BLOB NOT NULL CHECK(length(repository_id) = 16),
    generation_id BLOB NOT NULL CHECK(length(generation_id) = 20),
    producer_kind TEXT NOT NULL CHECK(producer_kind IN (
        'parser', 'compiler', 'scip', 'build_manifest', 'git', 'runtime_trace',
        'rule', 'heuristic', 'user_configuration', 'derivation'
    )),
    producer_name TEXT NOT NULL CHECK(length(producer_name) BETWEEN 1 AND 128),
    producer_version TEXT NOT NULL CHECK(length(producer_version) BETWEEN 1 AND 128),
    producer_configuration_hash BLOB NOT NULL CHECK(length(producer_configuration_hash) = 32),
    binary_digest BLOB NOT NULL CHECK(length(binary_digest) = 32),
    frontend_version TEXT CHECK(frontend_version IS NULL OR length(frontend_version) BETWEEN 1 AND 32768),
    language TEXT NOT NULL CHECK(length(language) BETWEEN 1 AND 32768),
    tier TEXT NOT NULL CHECK(tier IN ('tier_a', 'tier_b', 'tier_c', 'tier_d')),
    build_context_digest BLOB NOT NULL CHECK(length(build_context_digest) = 32),
    rule TEXT CHECK(rule IS NULL OR length(rule) BETWEEN 1 AND 32768),
    FOREIGN KEY(repository_id, generation_id)
        REFERENCES generation_meta(repository_id, generation_id)
        DEFERRABLE INITIALLY DEFERRED
) STRICT";

const FILES_SQL: &str = "CREATE TABLE files (
    file_id BLOB PRIMARY KEY NOT NULL CHECK(length(file_id) = 20),
    repository_id BLOB NOT NULL CHECK(length(repository_id) = 16),
    generation_id BLOB NOT NULL CHECK(length(generation_id) = 20),
    path TEXT NOT NULL CHECK(length(path) BETWEEN 1 AND 32768),
    content_hash BLOB NOT NULL CHECK(length(content_hash) = 32),
    byte_length INTEGER NOT NULL CHECK(byte_length >= 0),
    language TEXT NOT NULL CHECK(length(language) BETWEEN 1 AND 32768),
    encoding TEXT NOT NULL CHECK(length(encoding) BETWEEN 1 AND 32768),
    generated INTEGER NOT NULL CHECK(generated IN (0, 1)),
    provenance_id BLOB NOT NULL CHECK(length(provenance_id) = 20),
    evidence_source_ordinal INTEGER,
    UNIQUE(repository_id, generation_id, path),
    FOREIGN KEY(repository_id, generation_id)
        REFERENCES generation_meta(repository_id, generation_id)
        DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY(provenance_id) REFERENCES provenance(provenance_id)
        DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY(evidence_source_ordinal) REFERENCES source_refs(ordinal)
        DEFERRABLE INITIALLY DEFERRED
) STRICT";

const ENTITIES_SQL: &str = "CREATE TABLE entities (
    entity_id BLOB PRIMARY KEY NOT NULL CHECK(length(entity_id) = 20),
    repository_id BLOB NOT NULL CHECK(length(repository_id) = 16),
    generation_id BLOB NOT NULL CHECK(length(generation_id) = 20),
    kind TEXT NOT NULL CHECK(kind IN (
        'repository', 'worktree', 'package', 'build_target', 'directory', 'file',
        'module', 'namespace', 'class', 'struct', 'enum', 'union', 'type_alias',
        'trait', 'interface', 'protocol', 'function', 'method', 'constructor',
        'closure', 'field', 'property', 'constant', 'variable', 'parameter',
        'type_parameter', 'import', 'export', 'route', 'service', 'message_topic',
        'database_object', 'test', 'configuration_key', 'commit', 'change',
        'community_view', 'external_symbol'
    )),
    language TEXT NOT NULL CHECK(length(language) BETWEEN 1 AND 32768),
    tier TEXT NOT NULL CHECK(tier IN ('tier_a', 'tier_b', 'tier_c', 'tier_d')),
    canonical_name TEXT NOT NULL CHECK(length(canonical_name) <= 32768),
    display_name TEXT NOT NULL CHECK(length(display_name) <= 32768),
    qualified_name TEXT NOT NULL CHECK(length(qualified_name) <= 32768),
    container_kind TEXT CHECK(container_kind IN ('repository', 'file', 'entity')),
    container_id BLOB,
    visibility TEXT NOT NULL CHECK(visibility IN ('public', 'restricted', 'private', 'unknown')),
    provenance_id BLOB NOT NULL CHECK(length(provenance_id) = 20),
    evidence_source_ordinal INTEGER,
    CHECK((container_kind IS NULL AND container_id IS NULL)
       OR (container_kind IS NOT NULL AND container_id IS NOT NULL)),
    FOREIGN KEY(repository_id, generation_id)
        REFERENCES generation_meta(repository_id, generation_id)
        DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY(container_kind, container_id)
        REFERENCES identity_registry(kind, identity)
        DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY(provenance_id) REFERENCES provenance(provenance_id)
        DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY(evidence_source_ordinal) REFERENCES source_refs(ordinal)
        DEFERRABLE INITIALLY DEFERRED
) STRICT";

const ENTITY_FLAGS_SQL: &str = "CREATE TABLE entity_flags (
    entity_id BLOB NOT NULL CHECK(length(entity_id) = 20),
    flag TEXT NOT NULL CHECK(flag IN (
        'generated', 'external', 'test', 'exported', 'deprecated', 'synthetic'
    )),
    PRIMARY KEY(entity_id, flag),
    FOREIGN KEY(entity_id) REFERENCES entities(entity_id)
        DEFERRABLE INITIALLY DEFERRED
) STRICT";

const OCCURRENCES_SQL: &str = "CREATE TABLE occurrences (
    occurrence_id BLOB PRIMARY KEY NOT NULL CHECK(length(occurrence_id) = 20),
    repository_id BLOB NOT NULL CHECK(length(repository_id) = 16),
    generation_id BLOB NOT NULL CHECK(length(generation_id) = 20),
    file_id BLOB NOT NULL CHECK(length(file_id) = 20),
    source_ordinal INTEGER NOT NULL,
    role TEXT NOT NULL CHECK(role IN (
        'definition', 'declaration', 'reference', 'call_site', 'type_use',
        'import_use', 'write', 'read', 'inheritance_use', 'implementation_use',
        'decorator_use', 'macro_use', 'route_use', 'test_use', 'documentation',
        'string_evidence'
    )),
    enclosing_entity_id BLOB CHECK(enclosing_entity_id IS NULL OR length(enclosing_entity_id) = 20),
    target_kind TEXT NOT NULL CHECK(target_kind IN ('resolved', 'candidates', 'unresolved')),
    target_symbol_id BLOB CHECK(target_symbol_id IS NULL OR length(target_symbol_id) = 20),
    target_text_hash BLOB CHECK(target_text_hash IS NULL OR length(target_text_hash) = 32),
    target_total_count INTEGER CHECK(target_total_count IS NULL OR target_total_count >= 0),
    target_completeness TEXT CHECK(target_completeness IN ('complete', 'bounded', 'sampled', 'unknown')),
    syntactic_text_hash BLOB NOT NULL CHECK(length(syntactic_text_hash) = 32),
    syntax_kind TEXT NOT NULL CHECK(length(syntax_kind) BETWEEN 1 AND 32768),
    provenance_id BLOB NOT NULL CHECK(length(provenance_id) = 20),
    confidence INTEGER NOT NULL CHECK(confidence BETWEEN 0 AND 1000),
    evidence_source_ordinal INTEGER,
    CHECK((target_kind = 'resolved' AND target_symbol_id IS NOT NULL
           AND target_text_hash IS NULL AND target_total_count IS NULL
           AND target_completeness IS NULL)
       OR (target_kind = 'candidates' AND target_symbol_id IS NULL
           AND target_text_hash IS NULL AND target_total_count IS NOT NULL
           AND target_completeness IS NOT NULL)
       OR (target_kind = 'unresolved' AND target_symbol_id IS NULL
           AND target_text_hash IS NOT NULL AND target_total_count IS NULL
           AND target_completeness IS NULL)),
    FOREIGN KEY(repository_id, generation_id)
        REFERENCES generation_meta(repository_id, generation_id)
        DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY(file_id) REFERENCES files(file_id)
        DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY(source_ordinal) REFERENCES source_refs(ordinal)
        DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY(enclosing_entity_id) REFERENCES entities(entity_id)
        DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY(target_symbol_id) REFERENCES entities(entity_id)
        DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY(provenance_id) REFERENCES provenance(provenance_id)
        DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY(evidence_source_ordinal) REFERENCES source_refs(ordinal)
        DEFERRABLE INITIALLY DEFERRED
) STRICT";

const OCCURRENCE_CANDIDATES_SQL: &str = "CREATE TABLE occurrence_candidates (
    occurrence_id BLOB NOT NULL CHECK(length(occurrence_id) = 20),
    position INTEGER NOT NULL CHECK(position >= 0),
    entity_id BLOB NOT NULL CHECK(length(entity_id) = 20),
    PRIMARY KEY(occurrence_id, position),
    UNIQUE(occurrence_id, entity_id),
    FOREIGN KEY(occurrence_id) REFERENCES occurrences(occurrence_id)
        DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY(entity_id) REFERENCES entities(entity_id)
        DEFERRABLE INITIALLY DEFERRED
) STRICT";

const RELATIONS_SQL: &str = "CREATE TABLE relations (
    relation_id BLOB PRIMARY KEY NOT NULL CHECK(length(relation_id) = 20),
    repository_id BLOB NOT NULL CHECK(length(repository_id) = 16),
    generation_id BLOB NOT NULL CHECK(length(generation_id) = 20),
    subject_kind TEXT NOT NULL CHECK(subject_kind IN ('repository', 'file', 'entity', 'fact')),
    subject_id BLOB NOT NULL,
    predicate TEXT NOT NULL CHECK(predicate IN (
        'contains', 'declares', 'defines_at', 'refers_to', 'calls',
        'dispatch_candidate', 'imports', 'exports', 'uses_type', 'returns_type',
        'parameter_type', 'extends', 'implements', 'satisfies', 'embeds',
        'mixes_in', 'overrides', 'reads', 'writes', 'throws', 'handles_error',
        'tests', 'depends_on', 'calls_route', 'serves_route', 'publishes',
        'consumes', 'reads_table', 'writes_table', 'binds_to', 'calls_foreign',
        'generated_from', 'changed_in', 'lineage_renamed_from',
        'lineage_moved_from', 'lineage_split_from', 'lineage_merged_from',
        'co_changed_with', 'owned_by', 'member_of_view'
    )),
    object_kind TEXT NOT NULL CHECK(object_kind IN ('repository', 'file', 'entity', 'fact')),
    object_id BLOB NOT NULL,
    confidence INTEGER NOT NULL CHECK(confidence BETWEEN 0 AND 1000),
    evidence_kind TEXT NOT NULL CHECK(evidence_kind IN (
        'syntax', 'compiler', 'language_server', 'scip', 'derived'
    )),
    provenance_id BLOB NOT NULL CHECK(length(provenance_id) = 20),
    evidence_source_ordinal INTEGER,
    FOREIGN KEY(repository_id, generation_id)
        REFERENCES generation_meta(repository_id, generation_id)
        DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY(subject_kind, subject_id)
        REFERENCES identity_registry(kind, identity)
        DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY(object_kind, object_id)
        REFERENCES identity_registry(kind, identity)
        DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY(provenance_id) REFERENCES provenance(provenance_id)
        DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY(evidence_source_ordinal) REFERENCES source_refs(ordinal)
        DEFERRABLE INITIALLY DEFERRED
) STRICT";

const COVERAGE_SQL: &str = "CREATE TABLE coverage_records (
    coverage_id BLOB PRIMARY KEY NOT NULL CHECK(length(coverage_id) = 20),
    repository_id BLOB NOT NULL CHECK(length(repository_id) = 16),
    generation_id BLOB NOT NULL CHECK(length(generation_id) = 20),
    scope_kind TEXT NOT NULL CHECK(scope_kind IN ('repository', 'file', 'entity')),
    scope_id BLOB NOT NULL,
    domain TEXT NOT NULL CHECK(domain IN (
        'files', 'entities', 'occurrences', 'relations', 'provenance',
        'source_mappings', 'diagnostics', 'extensions'
    )),
    tier TEXT NOT NULL CHECK(tier IN ('tier_a', 'tier_b', 'tier_c', 'tier_d')),
    status TEXT NOT NULL CHECK(status IN ('complete', 'bounded', 'sampled', 'unknown')),
    discovered INTEGER NOT NULL CHECK(discovered >= 0),
    indexed INTEGER NOT NULL CHECK(indexed >= 0),
    skipped INTEGER NOT NULL CHECK(skipped >= 0),
    provenance_id BLOB NOT NULL CHECK(length(provenance_id) = 20),
    evidence_source_ordinal INTEGER,
    CHECK(indexed <= discovered AND skipped <= discovered - indexed),
    FOREIGN KEY(repository_id, generation_id)
        REFERENCES generation_meta(repository_id, generation_id)
        DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY(scope_kind, scope_id)
        REFERENCES identity_registry(kind, identity)
        DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY(provenance_id) REFERENCES provenance(provenance_id)
        DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY(evidence_source_ordinal) REFERENCES source_refs(ordinal)
        DEFERRABLE INITIALLY DEFERRED
) STRICT";

const SOURCE_MAPPINGS_TABLE_SQL: &str = "CREATE TABLE source_mappings (
    source_mapping_id BLOB PRIMARY KEY NOT NULL CHECK(length(source_mapping_id) = 20),
    repository_id BLOB NOT NULL CHECK(length(repository_id) = 16),
    generation_id BLOB NOT NULL CHECK(length(generation_id) = 20),
    provenance_id BLOB NOT NULL CHECK(length(provenance_id) = 20),
    payload TEXT NOT NULL CHECK(length(CAST(payload AS BLOB)) BETWEEN 2 AND 1048576),
    FOREIGN KEY(repository_id, generation_id)
        REFERENCES generation_meta(repository_id, generation_id)
        DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY(provenance_id) REFERENCES provenance(provenance_id)
        DEFERRABLE INITIALLY DEFERRED
) STRICT";

const SKIPPED_REGIONS_TABLE_SQL: &str = "CREATE TABLE skipped_regions (
    skipped_region_id BLOB PRIMARY KEY NOT NULL CHECK(length(skipped_region_id) = 20),
    repository_id BLOB NOT NULL CHECK(length(repository_id) = 16),
    generation_id BLOB NOT NULL CHECK(length(generation_id) = 20),
    provenance_id BLOB NOT NULL CHECK(length(provenance_id) = 20),
    payload TEXT NOT NULL CHECK(length(CAST(payload AS BLOB)) BETWEEN 2 AND 1048576),
    FOREIGN KEY(repository_id, generation_id)
        REFERENCES generation_meta(repository_id, generation_id)
        DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY(provenance_id) REFERENCES provenance(provenance_id)
        DEFERRABLE INITIALLY DEFERRED
) STRICT";

const DIAGNOSTICS_TABLE_SQL: &str = "CREATE TABLE diagnostics (
    diagnostic_id BLOB PRIMARY KEY NOT NULL CHECK(length(diagnostic_id) = 20),
    repository_id BLOB NOT NULL CHECK(length(repository_id) = 16),
    generation_id BLOB NOT NULL CHECK(length(generation_id) = 20),
    provenance_id BLOB NOT NULL CHECK(length(provenance_id) = 20),
    payload TEXT NOT NULL CHECK(length(CAST(payload AS BLOB)) BETWEEN 2 AND 1048576),
    FOREIGN KEY(repository_id, generation_id)
        REFERENCES generation_meta(repository_id, generation_id)
        DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY(provenance_id) REFERENCES provenance(provenance_id)
        DEFERRABLE INITIALLY DEFERRED
) STRICT";

const EXTENSIONS_TABLE_SQL: &str = "CREATE TABLE extensions (
    extension_id BLOB PRIMARY KEY NOT NULL CHECK(length(extension_id) = 20),
    repository_id BLOB NOT NULL CHECK(length(repository_id) = 16),
    generation_id BLOB NOT NULL CHECK(length(generation_id) = 20),
    provenance_id BLOB NOT NULL CHECK(length(provenance_id) = 20),
    payload TEXT NOT NULL CHECK(length(CAST(payload AS BLOB)) BETWEEN 2 AND 1048576),
    FOREIGN KEY(repository_id, generation_id)
        REFERENCES generation_meta(repository_id, generation_id)
        DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY(provenance_id) REFERENCES provenance(provenance_id)
        DEFERRABLE INITIALLY DEFERRED
) STRICT";

const EVIDENCE_DERIVATIONS_SQL: &str = "CREATE TABLE evidence_derivations (
    owner_kind TEXT NOT NULL CHECK(owner_kind IN ('file', 'entity', 'fact')),
    owner_id BLOB NOT NULL CHECK(length(owner_id) = 20),
    position INTEGER NOT NULL CHECK(position >= 0),
    reference_kind TEXT NOT NULL CHECK(reference_kind IN ('file', 'entity', 'fact')),
    reference_id BLOB NOT NULL CHECK(length(reference_id) = 20),
    PRIMARY KEY(owner_kind, owner_id, position),
    UNIQUE(owner_kind, owner_id, reference_kind, reference_id),
    FOREIGN KEY(owner_kind, owner_id)
        REFERENCES identity_registry(kind, identity)
        DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY(reference_kind, reference_id)
        REFERENCES identity_registry(kind, identity)
        DEFERRABLE INITIALLY DEFERRED
) STRICT";

const PROVENANCE_SOURCES_SQL: &str = "CREATE TABLE provenance_sources (
    provenance_id BLOB NOT NULL CHECK(length(provenance_id) = 20),
    source_kind TEXT NOT NULL CHECK(source_kind IN ('input', 'evidence')),
    position INTEGER NOT NULL CHECK(position >= 0),
    source_ordinal INTEGER NOT NULL CHECK(source_ordinal >= 0),
    PRIMARY KEY(provenance_id, source_kind, position),
    UNIQUE(provenance_id, source_kind, source_ordinal),
    FOREIGN KEY(provenance_id) REFERENCES provenance(provenance_id)
        DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY(source_ordinal) REFERENCES source_refs(ordinal)
        DEFERRABLE INITIALLY DEFERRED
) STRICT";

const PROVENANCE_DERIVATIONS_SQL: &str = "CREATE TABLE provenance_derivations (
    provenance_id BLOB NOT NULL CHECK(length(provenance_id) = 20),
    position INTEGER NOT NULL CHECK(position >= 0),
    reference_kind TEXT NOT NULL CHECK(reference_kind IN ('file', 'entity', 'fact')),
    reference_id BLOB NOT NULL CHECK(length(reference_id) = 20),
    PRIMARY KEY(provenance_id, position),
    UNIQUE(provenance_id, reference_kind, reference_id),
    FOREIGN KEY(provenance_id) REFERENCES provenance(provenance_id)
        DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY(reference_kind, reference_id)
        REFERENCES identity_registry(kind, identity)
        DEFERRABLE INITIALLY DEFERRED
) STRICT";

const FILE_PATH_INDEX_SQL: &str = "CREATE INDEX files_by_path ON files(path, file_id)";
const ENTITY_CANONICAL_NAME_INDEX_SQL: &str =
    "CREATE INDEX entities_by_canonical_name ON entities(canonical_name, entity_id)";
const ENTITY_QUALIFIED_NAME_INDEX_SQL: &str =
    "CREATE INDEX entities_by_qualified_name ON entities(qualified_name, entity_id)";
const OCCURRENCE_FILE_INDEX_SQL: &str =
    "CREATE INDEX occurrences_by_file ON occurrences(file_id, role, occurrence_id)";
const RELATION_SUBJECT_INDEX_SQL: &str = "CREATE INDEX relations_by_subject ON relations(subject_kind, subject_id, predicate, relation_id)";
const RELATION_OBJECT_INDEX_SQL: &str =
    "CREATE INDEX relations_by_object ON relations(object_kind, object_id, predicate, relation_id)";
const SOURCE_REF_FILE_INDEX_SQL: &str =
    "CREATE INDEX source_refs_by_file_span ON source_refs(file_id, start_byte, end_byte, ordinal)";
const COVERAGE_SCOPE_INDEX_SQL: &str =
    "CREATE INDEX coverage_by_scope ON coverage_records(scope_kind, scope_id, domain, coverage_id)";

const CONTROL_TABLES: [NamedSql; 2] = [
    NamedSql::table("application_meta", APPLICATION_META_SQL),
    NamedSql::table("migrations", MIGRATIONS_SQL),
];

const ORACLE_TABLES: [NamedSql; 19] = [
    NamedSql::table("application_meta", APPLICATION_META_SQL),
    NamedSql::table("migrations", MIGRATIONS_SQL),
    NamedSql::table("generation_meta", GENERATION_META_SQL),
    NamedSql::table("identity_registry", IDENTITY_REGISTRY_SQL),
    NamedSql::table("source_refs", SOURCE_REFS_SQL),
    NamedSql::table("provenance", PROVENANCE_SQL),
    NamedSql::table("files", FILES_SQL),
    NamedSql::table("entities", ENTITIES_SQL),
    NamedSql::table("entity_flags", ENTITY_FLAGS_SQL),
    NamedSql::table("occurrences", OCCURRENCES_SQL),
    NamedSql::table("occurrence_candidates", OCCURRENCE_CANDIDATES_SQL),
    NamedSql::table("relations", RELATIONS_SQL),
    NamedSql::table("source_mappings", SOURCE_MAPPINGS_TABLE_SQL),
    NamedSql::table("coverage_records", COVERAGE_SQL),
    NamedSql::table("skipped_regions", SKIPPED_REGIONS_TABLE_SQL),
    NamedSql::table("diagnostics", DIAGNOSTICS_TABLE_SQL),
    NamedSql::table("extensions", EXTENSIONS_TABLE_SQL),
    NamedSql::table("evidence_derivations", EVIDENCE_DERIVATIONS_SQL),
    NamedSql::table("provenance_sources", PROVENANCE_SOURCES_SQL),
];

const ORACLE_EXTRA_TABLES: [NamedSql; 1] = [NamedSql::table(
    "provenance_derivations",
    PROVENANCE_DERIVATIONS_SQL,
)];

const ORACLE_INDEXES: [NamedSql; 8] = [
    NamedSql::index("files_by_path", FILE_PATH_INDEX_SQL),
    NamedSql::index(
        "entities_by_canonical_name",
        ENTITY_CANONICAL_NAME_INDEX_SQL,
    ),
    NamedSql::index(
        "entities_by_qualified_name",
        ENTITY_QUALIFIED_NAME_INDEX_SQL,
    ),
    NamedSql::index("occurrences_by_file", OCCURRENCE_FILE_INDEX_SQL),
    NamedSql::index("relations_by_subject", RELATION_SUBJECT_INDEX_SQL),
    NamedSql::index("relations_by_object", RELATION_OBJECT_INDEX_SQL),
    NamedSql::index("source_refs_by_file_span", SOURCE_REF_FILE_INDEX_SQL),
    NamedSql::index("coverage_by_scope", COVERAGE_SCOPE_INDEX_SQL),
];

#[derive(Clone, Copy)]
struct NamedSql {
    kind: &'static str,
    name: &'static str,
    sql: &'static str,
}

impl NamedSql {
    const fn table(name: &'static str, sql: &'static str) -> Self {
        Self {
            kind: "table",
            name,
            sql,
        }
    }

    const fn index(name: &'static str, sql: &'static str) -> Self {
        Self {
            kind: "index",
            name,
            sql,
        }
    }
}

struct SchemaDefinition<'a> {
    application_id: u32,
    version: u32,
    kind: &'static [u8],
    allows_document_hash: bool,
    objects: &'a [NamedSql],
}

pub(crate) fn control_compatibility() -> SchemaCompatibility {
    compatibility(&control_definition())
}

pub(crate) fn oracle_compatibility() -> SchemaCompatibility {
    compatibility(&oracle_definition())
}

pub(crate) fn open_control(path: &Path) -> Result<Connection, CatalogError> {
    require_private_file_boundary(cfg!(test))?;
    let created = create_private_file(path, false)?;
    let flags = OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let connection = Connection::open_with_flags(path, flags).map_err(CatalogError::sqlite)?;
    configure_limits(&connection)?;
    verify_sqlite(&connection)?;
    let definition = control_definition();
    if created {
        configure_writer(&connection, JournalMode::Wal)?;
        initialize(&connection, &definition)?;
    } else {
        // Identity is checked before journal configuration because changing a
        // foreign file's journal mode would mutate data Rootlight does not own.
        configure_common(&connection)?;
        validate(&connection, &definition)?;
        configure_writer(&connection, JournalMode::Wal)?;
    }
    validate(&connection, &definition)?;
    install_authorizer(&connection)?;
    Ok(connection)
}

pub(crate) fn create_oracle(path: &Path) -> Result<Connection, CatalogError> {
    require_private_file_boundary(cfg!(test))?;
    create_private_file(path, true)?;
    let flags = OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let connection = Connection::open_with_flags(path, flags).map_err(CatalogError::sqlite)?;
    configure_limits(&connection)?;
    verify_sqlite(&connection)?;
    configure_writer(&connection, JournalMode::Delete)?;
    let definition = oracle_definition();
    initialize(&connection, &definition)?;
    validate(&connection, &definition)?;
    install_authorizer(&connection)?;
    Ok(connection)
}

pub(crate) fn open_oracle_reader(
    path: &Path,
    context: &GenerationContext<'_>,
) -> Result<Connection, CatalogError> {
    require_private_file_boundary(cfg!(test))?;
    validate_private_file(path)?;
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let connection = Connection::open_with_flags(path, flags).map_err(|error| {
        if matches!(&error, rusqlite::Error::SqliteFailure(source, _)
            if source.code == rusqlite::ffi::ErrorCode::CannotOpen)
        {
            CatalogError::with_source(
                CatalogErrorKind::NotFound,
                crate::CatalogErrorSource::Sqlite(error),
            )
        } else {
            CatalogError::sqlite(error)
        }
    })?;
    configure_limits(&connection)?;
    install_generation_cancellation(&connection, context)?;
    verify_sqlite(&connection)?;
    configure_reader(&connection)?;
    validate_schema(&connection, &oracle_definition())?;
    install_authorizer(&connection)?;
    Ok(connection)
}

pub(crate) fn install_generation_cancellation(
    connection: &Connection,
    context: &GenerationContext<'_>,
) -> Result<(), CatalogError> {
    let cancellation = context.cancellation().clone();
    connection
        .progress_handler(1_000, Some(move || cancellation.check().is_err()))
        .map_err(CatalogError::sqlite)
}

pub(crate) fn validate_control(connection: &Connection) -> Result<(), CatalogError> {
    validate(connection, &control_definition())
}

pub(crate) fn validate_oracle(connection: &Connection) -> Result<(), CatalogError> {
    validate(connection, &oracle_definition())
}

fn control_definition() -> SchemaDefinition<'static> {
    SchemaDefinition {
        application_id: CONTROL_APPLICATION_ID,
        version: CONTROL_SCHEMA_VERSION,
        kind: b"rootlight-control",
        allows_document_hash: false,
        objects: &CONTROL_TABLES,
    }
}

fn oracle_definition() -> SchemaDefinition<'static> {
    // The split keeps the primary table ledger readable while retaining one
    // canonical checksum order.
    static OBJECTS: std::sync::LazyLock<Vec<NamedSql>> = std::sync::LazyLock::new(|| {
        ORACLE_TABLES
            .into_iter()
            .chain(ORACLE_EXTRA_TABLES)
            .chain(ORACLE_INDEXES)
            .collect()
    });
    SchemaDefinition {
        application_id: ORACLE_APPLICATION_ID,
        version: ORACLE_SCHEMA_VERSION,
        kind: b"rootlight-oracle",
        allows_document_hash: true,
        objects: &OBJECTS,
    }
}

fn compatibility(definition: &SchemaDefinition<'_>) -> SchemaCompatibility {
    SchemaCompatibility::new(
        definition.application_id,
        definition.version,
        schema_checksum(definition),
    )
}

fn schema_checksum(definition: &SchemaDefinition<'_>) -> rootlight_ids::ContentHash {
    let input = definition
        .objects
        .iter()
        .map(|object| object.sql)
        .collect::<Vec<_>>()
        .join("\n");
    content_hash(input.as_bytes())
}

fn initialize(
    connection: &Connection,
    definition: &SchemaDefinition<'_>,
) -> Result<(), CatalogError> {
    let transaction = connection
        .unchecked_transaction()
        .map_err(CatalogError::sqlite)?;
    for object in definition.objects {
        transaction
            .execute_batch(object.sql)
            .map_err(CatalogError::sqlite)?;
    }
    let checksum = schema_checksum(definition);
    transaction
        .execute(
            "INSERT INTO application_meta(key, value) VALUES ('database_kind', ?1)",
            [definition.kind],
        )
        .map_err(CatalogError::sqlite)?;
    transaction
        .execute(
            "INSERT INTO migrations(migration_id, checksum) VALUES (?1, ?2)",
            params![definition.version, checksum.as_bytes().as_slice()],
        )
        .map_err(CatalogError::sqlite)?;
    transaction
        .pragma_update(None, "application_id", definition.application_id)
        .map_err(CatalogError::sqlite)?;
    transaction
        .pragma_update(None, "user_version", definition.version)
        .map_err(CatalogError::sqlite)?;
    transaction.commit().map_err(CatalogError::sqlite)
}

fn validate(
    connection: &Connection,
    definition: &SchemaDefinition<'_>,
) -> Result<(), CatalogError> {
    validate_schema(connection, definition)?;
    validate_integrity(connection)
}

fn validate_schema(
    connection: &Connection,
    definition: &SchemaDefinition<'_>,
) -> Result<(), CatalogError> {
    let application_id = pragma_u32(connection, "application_id")?;
    if application_id != definition.application_id {
        return Err(CatalogError::new(CatalogErrorKind::ForeignDatabase));
    }
    let version = pragma_u32(connection, "user_version")?;
    if version != definition.version {
        return Err(CatalogError::new(CatalogErrorKind::IncompatibleSchema));
    }
    validate_application_metadata(connection, definition)?;
    let kind: Option<Vec<u8>> = connection
        .query_row(
            "SELECT value
             FROM application_meta
             WHERE key = 'database_kind' AND length(value) BETWEEN 1 AND 128",
            [],
            |row| row.get(0),
        )
        .optional()
        .map_err(CatalogError::sqlite)?;
    if kind.as_deref() != Some(definition.kind) {
        return Err(CatalogError::new(CatalogErrorKind::ForeignDatabase));
    }
    validate_migration_ledger(connection, definition)?;
    let mut expected = definition
        .objects
        .iter()
        .map(|object| {
            (
                (object.kind.to_owned(), object.name.to_owned()),
                normalize_sql(object.sql),
            )
        })
        .collect::<BTreeMap<_, _>>();
    if expected.len() != definition.objects.len() {
        return Err(CatalogError::new(CatalogErrorKind::Corrupt));
    }
    let mut statement = connection
        .prepare(
            "SELECT type, name, sql
             FROM sqlite_schema
             WHERE sql IS NOT NULL
             ORDER BY type, name",
        )
        .map_err(CatalogError::sqlite)?;
    let mut rows = statement.query([]).map_err(CatalogError::sqlite)?;
    while let Some(row) = rows.next().map_err(CatalogError::sqlite)? {
        let kind: String = row.get(0).map_err(CatalogError::sqlite)?;
        let name: String = row.get(1).map_err(CatalogError::sqlite)?;
        let sql: String = row.get(2).map_err(CatalogError::sqlite)?;
        let Some(expected_sql) = expected.remove(&(kind, name)) else {
            return Err(CatalogError::new(CatalogErrorKind::Corrupt));
        };
        if normalize_sql(&sql) != expected_sql {
            return Err(CatalogError::new(CatalogErrorKind::Corrupt));
        }
    }
    if !expected.is_empty() {
        return Err(CatalogError::new(CatalogErrorKind::Corrupt));
    }
    Ok(())
}

fn validate_application_metadata(
    connection: &Connection,
    definition: &SchemaDefinition<'_>,
) -> Result<(), CatalogError> {
    let (row_count, kind_count, document_hash_count, valid_document_hash_count): (
        i64,
        i64,
        i64,
        i64,
    ) = connection
        .query_row(
            "SELECT
                count(*),
                coalesce(sum(key = 'database_kind'), 0),
                coalesce(sum(key = 'document_hash'), 0),
                coalesce(sum(key = 'document_hash' AND length(value) = 32), 0)
             FROM (SELECT key, value FROM application_meta LIMIT 3)",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .map_err(CatalogError::sqlite)?;
    let exact_control_ledger = row_count == 1 && kind_count == 1 && document_hash_count == 0;
    let exact_unsealed_oracle = row_count == 1 && kind_count == 1 && document_hash_count == 0;
    let exact_sealed_oracle = row_count == 2
        && kind_count == 1
        && document_hash_count == 1
        && valid_document_hash_count == 1;
    if (!definition.allows_document_hash && !exact_control_ledger)
        || (definition.allows_document_hash && !exact_unsealed_oracle && !exact_sealed_oracle)
    {
        return Err(CatalogError::new(CatalogErrorKind::Corrupt));
    }
    Ok(())
}

fn validate_migration_ledger(
    connection: &Connection,
    definition: &SchemaDefinition<'_>,
) -> Result<(), CatalogError> {
    let checksum = schema_checksum(definition);
    let (row_count, matching_count): (i64, i64) = connection
        .query_row(
            "SELECT
                count(*),
                coalesce(sum(migration_id = ?1 AND checksum = ?2), 0)
             FROM (SELECT migration_id, checksum FROM migrations LIMIT 2)",
            params![definition.version, checksum.as_bytes().as_slice(),],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(CatalogError::sqlite)?;
    if row_count != 1 {
        return Err(CatalogError::new(CatalogErrorKind::Corrupt));
    }
    if matching_count != 1 {
        return Err(CatalogError::new(
            CatalogErrorKind::MigrationChecksumMismatch,
        ));
    }
    Ok(())
}

fn validate_integrity(connection: &Connection) -> Result<(), CatalogError> {
    let quick: String = connection
        .query_row("PRAGMA quick_check(1)", [], |row| row.get(0))
        .map_err(CatalogError::sqlite)?;
    if quick != "ok" {
        return Err(CatalogError::new(CatalogErrorKind::Corrupt));
    }
    let mut statement = connection
        .prepare("PRAGMA foreign_key_check")
        .map_err(CatalogError::sqlite)?;
    let mut rows = statement.query([]).map_err(CatalogError::sqlite)?;
    if rows.next().map_err(CatalogError::sqlite)?.is_some() {
        return Err(CatalogError::new(CatalogErrorKind::Corrupt));
    }
    Ok(())
}

fn normalize_sql(sql: &str) -> String {
    sql.trim()
        .trim_end_matches(';')
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn verify_sqlite(connection: &Connection) -> Result<(), CatalogError> {
    if rusqlite::version_number() < MIN_SQLITE_VERSION_NUMBER {
        return Err(CatalogError::new(CatalogErrorKind::UnsupportedSqlite));
    }
    let compile_options = connection
        .prepare("PRAGMA compile_options")
        .map_err(CatalogError::sqlite)?
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(CatalogError::sqlite)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(CatalogError::sqlite)?;
    if compile_options
        .iter()
        .any(|option| option == "OMIT_FOREIGN_KEY")
    {
        return Err(CatalogError::new(CatalogErrorKind::UnsupportedSqlite));
    }
    Ok(())
}

fn configure_limits(connection: &Connection) -> Result<(), CatalogError> {
    for (limit, value) in [
        (Limit::SQLITE_LIMIT_LENGTH, 2 * 1024 * 1024),
        (Limit::SQLITE_LIMIT_SQL_LENGTH, 64 * 1024),
        (Limit::SQLITE_LIMIT_COLUMN, 64),
        (Limit::SQLITE_LIMIT_EXPR_DEPTH, 128),
        (Limit::SQLITE_LIMIT_COMPOUND_SELECT, 32),
        (Limit::SQLITE_LIMIT_VDBE_OP, 100_000),
        (Limit::SQLITE_LIMIT_FUNCTION_ARG, 32),
        (Limit::SQLITE_LIMIT_ATTACHED, 0),
        (Limit::SQLITE_LIMIT_LIKE_PATTERN_LENGTH, 256),
        (Limit::SQLITE_LIMIT_VARIABLE_NUMBER, 64),
        (Limit::SQLITE_LIMIT_TRIGGER_DEPTH, 0),
        (Limit::SQLITE_LIMIT_WORKER_THREADS, 0),
    ] {
        connection
            .set_limit(limit, value)
            .map_err(CatalogError::sqlite)?;
        if connection.limit(limit).map_err(CatalogError::sqlite)? != value {
            return Err(CatalogError::new(
                CatalogErrorKind::UnsupportedConfiguration,
            ));
        }
    }
    Ok(())
}

fn configure_writer(
    connection: &Connection,
    journal_mode: JournalMode,
) -> Result<(), CatalogError> {
    configure_common(connection)?;
    let observed: String = connection
        .pragma_update_and_check(None, "journal_mode", journal_mode.requested(), |row| {
            row.get(0)
        })
        .map_err(CatalogError::sqlite)?;
    if !observed.eq_ignore_ascii_case(journal_mode.observed()) {
        return Err(CatalogError::new(
            CatalogErrorKind::UnsupportedConfiguration,
        ));
    }
    connection
        .execute_batch(
            "PRAGMA synchronous = FULL;
             PRAGMA wal_autocheckpoint = 256;
             PRAGMA temp_store = MEMORY;
             PRAGMA cell_size_check = ON;",
        )
        .map_err(CatalogError::sqlite)?;
    validate_connection(connection, journal_mode)
}

fn configure_reader(connection: &Connection) -> Result<(), CatalogError> {
    configure_common(connection)?;
    connection
        .execute_batch(
            "PRAGMA query_only = ON;
             PRAGMA temp_store = MEMORY;
             PRAGMA cell_size_check = ON;",
        )
        .map_err(CatalogError::sqlite)?;
    let query_only: i64 = connection
        .query_row("PRAGMA query_only", [], |row| row.get(0))
        .map_err(CatalogError::sqlite)?;
    if query_only != 1 {
        return Err(CatalogError::new(
            CatalogErrorKind::UnsupportedConfiguration,
        ));
    }
    Ok(())
}

fn configure_common(connection: &Connection) -> Result<(), CatalogError> {
    connection
        .busy_timeout(Duration::from_millis(SQLITE_BUSY_TIMEOUT_MS))
        .map_err(CatalogError::sqlite)?;
    for (configuration, enabled) in [
        (DbConfig::SQLITE_DBCONFIG_ENABLE_FKEY, true),
        (DbConfig::SQLITE_DBCONFIG_DEFENSIVE, true),
        (DbConfig::SQLITE_DBCONFIG_TRUSTED_SCHEMA, false),
        (DbConfig::SQLITE_DBCONFIG_DQS_DDL, false),
        (DbConfig::SQLITE_DBCONFIG_DQS_DML, false),
        (DbConfig::SQLITE_DBCONFIG_ENABLE_ATTACH_CREATE, false),
        (DbConfig::SQLITE_DBCONFIG_ENABLE_ATTACH_WRITE, false),
    ] {
        let observed = connection
            .set_db_config(configuration, enabled)
            .map_err(CatalogError::sqlite)?;
        if observed != enabled {
            return Err(CatalogError::new(
                CatalogErrorKind::UnsupportedConfiguration,
            ));
        }
    }
    connection
        .execute_batch("PRAGMA foreign_keys = ON; PRAGMA trusted_schema = OFF;")
        .map_err(CatalogError::sqlite)
}

fn validate_connection(
    connection: &Connection,
    journal_mode: JournalMode,
) -> Result<(), CatalogError> {
    let observed_journal: String = connection
        .query_row("PRAGMA journal_mode", [], |row| row.get(0))
        .map_err(CatalogError::sqlite)?;
    let synchronous: i64 = connection
        .query_row("PRAGMA synchronous", [], |row| row.get(0))
        .map_err(CatalogError::sqlite)?;
    let foreign_keys: i64 = connection
        .query_row("PRAGMA foreign_keys", [], |row| row.get(0))
        .map_err(CatalogError::sqlite)?;
    let trusted_schema: i64 = connection
        .query_row("PRAGMA trusted_schema", [], |row| row.get(0))
        .map_err(CatalogError::sqlite)?;
    if !observed_journal.eq_ignore_ascii_case(journal_mode.observed())
        || synchronous != 2
        || foreign_keys != 1
        || trusted_schema != 0
        || !connection
            .db_config(DbConfig::SQLITE_DBCONFIG_DEFENSIVE)
            .map_err(CatalogError::sqlite)?
        || connection
            .db_config(DbConfig::SQLITE_DBCONFIG_DQS_DDL)
            .map_err(CatalogError::sqlite)?
        || connection
            .db_config(DbConfig::SQLITE_DBCONFIG_DQS_DML)
            .map_err(CatalogError::sqlite)?
    {
        return Err(CatalogError::new(
            CatalogErrorKind::UnsupportedConfiguration,
        ));
    }
    Ok(())
}

fn install_authorizer(connection: &Connection) -> Result<(), CatalogError> {
    connection
        .authorizer(Some(
            |context: rusqlite::hooks::AuthContext<'_>| match context.action {
                AuthAction::Attach { .. }
                | AuthAction::Detach { .. }
                | AuthAction::CreateTempIndex { .. }
                | AuthAction::CreateTempTable { .. }
                | AuthAction::CreateTempTrigger { .. }
                | AuthAction::CreateTempView { .. }
                | AuthAction::DropTempIndex { .. }
                | AuthAction::DropTempTable { .. }
                | AuthAction::DropTempTrigger { .. }
                | AuthAction::DropTempView { .. }
                | AuthAction::CreateVtable { .. }
                | AuthAction::DropVtable { .. } => Authorization::Deny,
                _ => Authorization::Allow,
            },
        ))
        .map_err(CatalogError::sqlite)
}

fn pragma_u32(connection: &Connection, pragma: &str) -> Result<u32, CatalogError> {
    let sql = match pragma {
        "application_id" => "PRAGMA application_id",
        "user_version" => "PRAGMA user_version",
        _ => return Err(CatalogError::new(CatalogErrorKind::Corrupt)),
    };
    let value: i64 = connection
        .query_row(sql, [], |row| row.get(0))
        .map_err(CatalogError::sqlite)?;
    u32::try_from(value).map_err(|_| CatalogError::new(CatalogErrorKind::Corrupt))
}

fn require_private_file_boundary(test_scaffold: bool) -> Result<(), CatalogError> {
    if test_scaffold {
        Ok(())
    } else {
        Err(CatalogError::new(
            CatalogErrorKind::UnsupportedPrivateFileBoundary,
        ))
    }
}

fn create_private_file(path: &Path, exclusive: bool) -> Result<bool, CatalogError> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    match options.open(path) {
        Ok(file) => {
            drop(file);
            validate_private_file(path)?;
            Ok(true)
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists && !exclusive => {
            validate_private_file(path)?;
            Ok(false)
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            Err(CatalogError::io(CatalogErrorKind::AlreadyExists, error))
        }
        Err(error) => Err(CatalogError::io(CatalogErrorKind::Storage, error)),
    }
}

fn validate_private_file(path: &Path) -> Result<(), CatalogError> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| CatalogError::io(CatalogErrorKind::NotFound, error))?;
    if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
        return Err(CatalogError::new(CatalogErrorKind::InsecureFile));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        if metadata.mode() & 0o077 != 0 {
            return Err(CatalogError::new(CatalogErrorKind::InsecureFile));
        }
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum JournalMode {
    Wal,
    Delete,
}

impl JournalMode {
    const fn requested(self) -> &'static str {
        match self {
            Self::Wal => "WAL",
            Self::Delete => "DELETE",
        }
    }

    const fn observed(self) -> &'static str {
        match self {
            Self::Wal => "wal",
            Self::Delete => "delete",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_checksums_are_distinct_and_stable_width() {
        let control = control_compatibility();
        let oracle = oracle_compatibility();

        assert_ne!(control.checksum(), oracle.checksum());
        assert_eq!(control.checksum().as_bytes().len(), 32);
        assert_eq!(oracle.checksum().as_bytes().len(), 32);
    }

    #[test]
    fn proposed_private_file_boundary_fails_closed() {
        let error =
            require_private_file_boundary(false).expect_err("proposed boundary is unavailable");

        assert_eq!(
            error.kind(),
            CatalogErrorKind::UnsupportedPrivateFileBoundary
        );
    }
}
