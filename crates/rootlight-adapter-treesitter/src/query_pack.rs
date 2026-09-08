//! Reviewed structural query packs for Rootlight's audited grammars.
//!
//! Native queries and capture indices stay private; runtime extraction sees
//! only the closed, parser-independent role mapping defined here.

use std::{cmp::Ordering, collections::BinaryHeap, ops::ControlFlow};

use rootlight_adapter_sdk::{
    AdapterError, DiagnosticCode, ResourceKind, SinkError, SyntaxFactKind,
};
use rootlight_cancel::Cancellation;
use tree_sitter::{
    CaptureQuantifier, Query, QueryCapture, QueryCursor, QueryCursorOptions, StreamingIterator,
};

use crate::{
    GrammarFamily,
    registry::{language_for, native_family_for_source},
};

mod r;
mod scala;
mod sql;
mod yaml;

const QUERY_CURSOR_MATCH_LIMIT: u32 = 4096;
const HARD_MAX_QUERY_MATCHES: usize = 1_048_576;
const HARD_MAX_QUERY_CAPTURES: usize = 2_097_152;
const HARD_MAX_QUERY_FACTS: usize = 1_048_576;
// Optional query work stays proportional to the caller's emission budget;
// identity patterns have their own hard-bounded scan below.
const QUERY_MATCHES_PER_RETAINED_FACT: usize = 64;
const QUERY_CAPTURES_PER_RETAINED_FACT: usize = 128;

const EXPECTED_CAPTURES: [&str; 12] = [
    "call",
    "comment",
    "declaration",
    "definition",
    "documentation",
    "import",
    "module",
    "reference",
    "root",
    "scope",
    "signature",
    "string",
];
const RUST_SPECIAL_CAPTURES: [&str; 4] =
    ["scope_trait", "scope_type", "scoped_call", "test_attribute"];
const TERMINAL_CALL_NAME_CAPTURE: &str = "call_name";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum StructuralRole {
    Root,
    Module,
    Declaration,
    Signature,
    Import,
    Scope,
    ScopeTrait,
    ScopeType,
    Definition,
    Call,
    CallName,
    ScopedCall,
    Reference,
    Comment,
    Documentation,
    StringLiteral,
    TestAttribute,
}

impl StructuralRole {
    fn from_capture_name(name: &str) -> Option<Self> {
        match name {
            "root" => Some(Self::Root),
            "module" => Some(Self::Module),
            "declaration" => Some(Self::Declaration),
            "signature" => Some(Self::Signature),
            "import" => Some(Self::Import),
            "scope" => Some(Self::Scope),
            "scope_trait" => Some(Self::ScopeTrait),
            "scope_type" => Some(Self::ScopeType),
            "definition" => Some(Self::Definition),
            "call" => Some(Self::Call),
            "call_name" => Some(Self::CallName),
            "scoped_call" => Some(Self::ScopedCall),
            "reference" => Some(Self::Reference),
            "comment" => Some(Self::Comment),
            "documentation" => Some(Self::Documentation),
            "string" => Some(Self::StringLiteral),
            "test_attribute" => Some(Self::TestAttribute),
            _ => None,
        }
    }

    pub(crate) const fn fact_kind(self) -> SyntaxFactKind {
        match self {
            Self::Root => SyntaxFactKind::Root,
            Self::Module => SyntaxFactKind::Module,
            Self::Declaration => SyntaxFactKind::Declaration,
            Self::Signature | Self::ScopeTrait | Self::ScopeType | Self::TestAttribute => {
                SyntaxFactKind::Signature
            }
            Self::Import => SyntaxFactKind::Import,
            Self::Scope => SyntaxFactKind::Scope,
            Self::Definition | Self::Call | Self::CallName | Self::ScopedCall | Self::Reference => {
                SyntaxFactKind::Occurrence
            }
            Self::Comment | Self::Documentation => SyntaxFactKind::Comment,
            Self::StringLiteral => SyntaxFactKind::StringLiteral,
        }
    }

    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Root => "root",
            Self::Module => "module",
            Self::Declaration => "declaration",
            Self::Signature => "signature",
            Self::Import => "import",
            Self::Scope => "scope",
            Self::ScopeTrait => "scope_trait",
            Self::ScopeType => "scope_type",
            Self::Definition => "definition",
            Self::Call => "call",
            Self::CallName => "call_name",
            Self::ScopedCall => "scoped_call",
            Self::Reference => "reference",
            Self::Comment => "comment",
            Self::Documentation => "documentation",
            Self::StringLiteral => "string",
            Self::TestAttribute => "test_attribute",
        }
    }

    pub(crate) const fn container_rank(self) -> Option<u8> {
        match self {
            Self::Root => Some(0),
            Self::Module => Some(1),
            Self::Scope => Some(2),
            Self::Declaration => Some(3),
            _ => None,
        }
    }

    const fn retention_rank(self) -> u8 {
        match self {
            Self::Root => 0,
            Self::Module => 1,
            Self::Declaration | Self::Definition => 2,
            Self::Signature | Self::Scope | Self::ScopeTrait | Self::ScopeType => 3,
            Self::TestAttribute => 4,
            Self::Import => 5,
            Self::Documentation => 6,
            Self::Call | Self::ScopedCall => 7,
            // A retained terminal name is safe only when its containing call
            // survived the same bounded extraction.
            Self::CallName | Self::Reference => 8,
            Self::Comment => 9,
            Self::StringLiteral => 10,
        }
    }

    const fn belongs_to_identity_closure(self) -> bool {
        matches!(
            self,
            Self::Root
                | Self::Module
                | Self::Declaration
                | Self::Signature
                | Self::Scope
                | Self::ScopeTrait
                | Self::ScopeType
                | Self::Definition
                | Self::TestAttribute
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum QueryLimit {
    Match,
    Capture,
    Fact,
    CursorMatch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct QueryCandidate {
    pub(crate) start: usize,
    pub(crate) end: usize,
    pub(crate) role: StructuralRole,
    pub(crate) syntax: &'static str,
    pub(crate) required: bool,
    // Native nesting disambiguates YAML nodes with identical byte ranges.
    pub(crate) native_depth: usize,
}

impl QueryCandidate {
    pub(crate) fn retention_rank(self) -> (u8, usize, usize, StructuralRole, &'static str, usize) {
        (
            self.role.retention_rank(),
            self.start,
            self.end,
            self.role,
            self.syntax,
            self.native_depth,
        )
    }

    pub(crate) fn selection_rank(
        self,
    ) -> (u8, u8, usize, usize, StructuralRole, &'static str, usize) {
        let (role, start, end, structural_role, syntax, native_depth) = self.retention_rank();
        (
            u8::from(!self.required),
            role,
            start,
            end,
            structural_role,
            syntax,
            native_depth,
        )
    }

    pub(crate) fn same_fact(self, other: Self) -> bool {
        self.start == other.start
            && self.end == other.end
            && self.role == other.role
            && self.syntax == other.syntax
            && self.native_depth == other.native_depth
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RetainedCandidate(QueryCandidate);

impl RetainedCandidate {
    fn rank(self) -> (u8, usize, usize, StructuralRole, &'static str, usize) {
        self.0.retention_rank()
    }
}

impl Ord for RetainedCandidate {
    fn cmp(&self, other: &Self) -> Ordering {
        self.rank().cmp(&other.rank())
    }
}

impl PartialOrd for RetainedCandidate {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

pub(crate) struct QueryExtraction {
    pub(crate) candidates: Vec<QueryCandidate>,
    pub(crate) limit: Option<QueryLimit>,
    pub(crate) fact_limit: usize,
}

#[derive(Clone, Copy)]
struct QueryInput<'a> {
    family: GrammarFamily,
    tree: &'a tree_sitter::Tree,
    source: &'a [u8],
    cancellation: &'a Cancellation,
}

#[derive(Clone, Copy)]
struct QueryScanLimits {
    matches: usize,
    captures: usize,
}

pub(crate) struct QueryPack {
    // Optional query work may stop proportionally to the caller's output
    // budget. Identity patterns stay separate so that stopping optional work
    // can never strand a retained declaration without its defining evidence.
    identity_query: Query,
    optional_query: Query,
    roles_by_capture: Vec<StructuralRole>,
}

impl QueryPack {
    fn compile(family: GrammarFamily, source: &str) -> Result<Self, GrammarFamily> {
        Self::compile_native(family, family, source)
    }

    fn compile_native(
        family: GrammarFamily,
        native_family: GrammarFamily,
        source: &str,
    ) -> Result<Self, GrammarFamily> {
        let language = language_for(native_family);
        let mut identity_query = Query::new(&language, source).map_err(|_| family)?;
        let mut optional_query = Query::new(&language, source).map_err(|_| family)?;
        let mut expected = EXPECTED_CAPTURES.to_vec();
        if matches!(family, GrammarFamily::Json | GrammarFamily::Toml) {
            expected.retain(|name| !matches!(*name, "call" | "reference" | "signature" | "import"));
        }
        if family == GrammarFamily::Yaml {
            expected.retain(|name| !matches!(*name, "call" | "import"));
        }
        if family == GrammarFamily::Html {
            expected.retain(|name| !matches!(*name, "call" | "import" | "reference" | "scope"));
        }
        if family == GrammarFamily::Sql {
            expected.retain(|name| !matches!(*name, "call" | "import" | "scope"));
        }
        if matches!(
            family,
            GrammarFamily::Lua | GrammarFamily::Ruby | GrammarFamily::Bash | GrammarFamily::R
        ) {
            // Runtime module-loading calls are not grammar import statements.
            expected.retain(|name| *name != "import");
        }
        if family == GrammarFamily::Css {
            expected.retain(|name| !matches!(*name, "call" | "reference" | "signature"));
            expected.push("scope_type");
        }
        if family == GrammarFamily::Rust {
            expected.extend(RUST_SPECIAL_CAPTURES);
            expected.sort_unstable();
        } else {
            if family == GrammarFamily::Swift {
                expected.extend(["scope_trait", "scope_type"]);
            }
            if supports_terminal_call_name(family) {
                expected.push(TERMINAL_CALL_NAME_CAPTURE);
            }
            if supports_test_attribute(family) {
                expected.push("test_attribute");
            }
            expected.sort_unstable();
        }
        let mut observed = identity_query.capture_names().to_vec();
        observed.sort_unstable();
        if observed != expected {
            return Err(family);
        }
        let roles_by_capture = identity_query
            .capture_names()
            .iter()
            .map(|name| StructuralRole::from_capture_name(name).ok_or(family))
            .collect::<Result<Vec<_>, _>>()?;
        for pattern in 0..identity_query.pattern_count() {
            let has_identity_capture = identity_query
                .capture_quantifiers(pattern)
                .iter()
                .zip(&roles_by_capture)
                .any(|(quantifier, role)| {
                    *quantifier != CaptureQuantifier::Zero && role.belongs_to_identity_closure()
                });
            if has_identity_capture {
                optional_query.disable_pattern(pattern);
            } else {
                identity_query.disable_pattern(pattern);
            }
        }
        let pack = Self {
            identity_query,
            optional_query,
            roles_by_capture,
        };
        if (0..u32::try_from(pack.roles_by_capture.len()).map_err(|_| family)?)
            .any(|capture| pack.role_for_capture(capture).is_none())
        {
            return Err(family);
        }
        Ok(pack)
    }

    pub(crate) fn role_for_capture(&self, capture: u32) -> Option<StructuralRole> {
        usize::try_from(capture)
            .ok()
            .and_then(|index| self.roles_by_capture.get(index))
            .copied()
    }

    pub(crate) fn extract(
        &self,
        family: GrammarFamily,
        tree: &tree_sitter::Tree,
        source: &[u8],
        max_nodes: usize,
        max_facts: usize,
        cancellation: &Cancellation,
    ) -> Result<QueryExtraction, AdapterError> {
        cancellation.check()?;
        let max_facts = max_facts.min(HARD_MAX_QUERY_FACTS);
        let identity_limits = identity_scan_limits(max_nodes)?;
        let mut identity_candidates =
            self.extract_identity(family, tree, source, max_nodes, cancellation)?;

        let budgeted_matches = max_facts
            .checked_mul(QUERY_MATCHES_PER_RETAINED_FACT)
            .ok_or_else(|| query_failure("query-match-accounting"))?;
        let budgeted_captures = max_facts
            .checked_mul(QUERY_CAPTURES_PER_RETAINED_FACT)
            .ok_or_else(|| query_failure("query-capture-accounting"))?;
        let optional_max_matches = identity_limits.matches.min(budgeted_matches);
        let optional_max_captures = identity_limits.captures.min(budgeted_captures);
        if optional_max_matches == 0 || optional_max_captures == 0 || max_facts == 0 {
            return Ok(QueryExtraction {
                candidates: identity_candidates,
                limit: Some(QueryLimit::Fact),
                fact_limit: max_facts,
            });
        }

        let mut candidates = BinaryHeap::new();
        candidates
            .try_reserve(max_facts.min(4096))
            .map_err(|_| AdapterError::Sink(SinkError::AllocationFailed))?;
        let mut fact_limit_reached = false;
        let mut limit = self.scan_query(
            &self.optional_query,
            QueryInput {
                family,
                tree,
                source,
                cancellation,
            },
            QueryScanLimits {
                matches: optional_max_matches,
                captures: optional_max_captures,
            },
            |candidate| {
                let candidate = RetainedCandidate(candidate);
                if candidates.len() < max_facts {
                    candidates.push(candidate);
                    return Ok(());
                }

                fact_limit_reached = true;
                // Continue the bounded optional query after capacity is reached
                // so late imports or documentation can replace lower-value
                // calls and references.
                let mut worst = candidates
                    .peek_mut()
                    .expect("a nonzero full fact heap has a worst candidate");
                if candidate < *worst {
                    *worst = candidate;
                }
                Ok(())
            },
        )?;
        if limit.is_none() && fact_limit_reached {
            limit = Some(QueryLimit::Fact);
        }
        identity_candidates
            .try_reserve(candidates.len())
            .map_err(|_| AdapterError::Sink(SinkError::AllocationFailed))?;
        identity_candidates.extend(candidates.into_iter().map(|candidate| candidate.0));
        Ok(QueryExtraction {
            candidates: identity_candidates,
            limit,
            fact_limit: max_facts,
        })
    }

    pub(crate) fn extract_identity(
        &self,
        family: GrammarFamily,
        tree: &tree_sitter::Tree,
        source: &[u8],
        max_nodes: usize,
        cancellation: &Cancellation,
    ) -> Result<Vec<QueryCandidate>, AdapterError> {
        cancellation.check()?;
        let limits = identity_scan_limits(max_nodes)?;
        let mut candidates = Vec::new();
        let limit = self.scan_query(
            &self.identity_query,
            QueryInput {
                family,
                tree,
                source,
                cancellation,
            },
            limits,
            |mut candidate| {
                if candidate.role.belongs_to_identity_closure() {
                    // Ordinary scopes become mandatory only when they contain a
                    // declaration; runtime marks those after deduplication.
                    // Data scalar siblings are needed to count array positions,
                    // even when they contain no named declaration themselves.
                    candidate.required = candidate.role != StructuralRole::Scope
                        || matches!(
                            family,
                            GrammarFamily::Json | GrammarFamily::Toml | GrammarFamily::Yaml
                        );
                    push_candidate_fallible(&mut candidates, candidate, limits.captures)?;
                }
                Ok(())
            },
        )?;
        if let Some(limit) = limit {
            return Err(identity_scan_limit(limit, limits.matches, limits.captures));
        }
        Ok(candidates)
    }

    fn scan_query(
        &self,
        query: &Query,
        input: QueryInput<'_>,
        limits: QueryScanLimits,
        mut retain: impl FnMut(QueryCandidate) -> Result<(), AdapterError>,
    ) -> Result<Option<QueryLimit>, AdapterError> {
        let scala_package_prefix_end = if input.family == GrammarFamily::Scala {
            scala::leading_package_end(input.tree.root_node(), input.cancellation)?
        } else {
            None
        };
        let mut cursor = QueryCursor::new();
        cursor.set_match_limit(QUERY_CURSOR_MATCH_LIMIT);
        let mut callback_cancelled = false;
        let mut progress = |_: &tree_sitter::QueryCursorState| {
            if input.cancellation.check().is_ok() {
                ControlFlow::Continue(())
            } else {
                callback_cancelled = true;
                ControlFlow::Break(())
            }
        };
        let options = QueryCursorOptions::new().progress_callback(&mut progress);
        let mut matches =
            cursor.matches_with_options(query, input.tree.root_node(), input.source, options);
        let mut match_count = 0usize;
        let mut capture_count = 0usize;
        let mut limit = None;

        'query: while let Some(query_match) = matches.next() {
            if match_count >= limits.matches {
                limit = Some(QueryLimit::Match);
                break;
            }
            match_count = match_count
                .checked_add(1)
                .ok_or_else(|| query_failure("query-match-accounting"))?;
            for capture in query_match.captures {
                if capture_count >= limits.captures {
                    limit = Some(QueryLimit::Capture);
                    break 'query;
                }
                capture_count = capture_count
                    .checked_add(1)
                    .ok_or_else(|| query_failure("query-capture-accounting"))?;
                let role = self
                    .role_for_capture(capture.index)
                    .ok_or_else(|| query_failure("query-capture-role"))?;
                if input.family == GrammarFamily::Lua
                    && role == StructuralRole::Reference
                    && lua_nonlexical_identifier(capture.node)
                {
                    // Member accesses retain their complete qualified capture;
                    // literal keys, labels and binding attributes are not lexical reads.
                    continue;
                }
                let mut capture = *capture;
                if input.family == GrammarFamily::Scala
                    && !scala::retain_capture(capture.node, role, input.source)
                {
                    continue;
                }
                if input.family == GrammarFamily::R {
                    if matches!(
                        role,
                        StructuralRole::Declaration
                            | StructuralRole::Definition
                            | StructuralRole::Signature
                    ) {
                        let Some(binding) = r::declaration(capture.node, input.source) else {
                            continue;
                        };
                        if role == StructuralRole::Signature && binding.function.is_none() {
                            continue;
                        }
                        if role == StructuralRole::Definition {
                            let Some(name) = binding.name else {
                                continue;
                            };
                            capture.node = name;
                        }
                    } else if role == StructuralRole::Reference
                        && r::is_nonlexical_name(capture.node, input.source)
                    {
                        continue;
                    }
                }
                if input.family == GrammarFamily::Sql
                    && role == StructuralRole::Reference
                    && sql::is_declared_name(capture.node)
                {
                    continue;
                }
                if input.family == GrammarFamily::Sql && role == StructuralRole::Definition {
                    let Some(name) = sql::definition_node(capture.node) else {
                        continue;
                    };
                    capture.node = name;
                }
                let mut candidate =
                    candidate_for_capture(input.family, capture, role, input.source)?;
                if input.family == GrammarFamily::Scala
                    && matches!(role, StructuralRole::Scope | StructuralRole::Declaration)
                    && capture.node.kind() == "package_clause"
                    && capture.node.child_by_field_name("body").is_none()
                {
                    if scala_package_prefix_end.is_some_and(|end| capture.node.end_byte() <= end)
                        && capture.node.parent() == Some(input.tree.root_node())
                    {
                        // Leading unbraced Scala packages wrap the remaining unit,
                        // just like nested explicit packagings (SLS 9). Only the
                        // container expands; written name evidence stays unchanged.
                        candidate.end = input.tree.root_node().end_byte();
                    } else if role == StructuralRole::Scope {
                        candidate.syntax = "scala.unbraced_package_unavailable";
                    }
                }
                retain(candidate)?;
            }
        }
        drop(matches);
        if callback_cancelled {
            input.cancellation.check()?;
        }
        input.cancellation.check()?;
        if cursor.did_exceed_match_limit() {
            Ok(Some(QueryLimit::CursorMatch))
        } else {
            Ok(limit)
        }
    }
}

fn identity_scan_limits(max_nodes: usize) -> Result<QueryScanLimits, AdapterError> {
    Ok(QueryScanLimits {
        matches: max_nodes
            .checked_mul(8)
            .ok_or_else(|| query_failure("query-match-accounting"))?
            .min(HARD_MAX_QUERY_MATCHES),
        captures: max_nodes
            .checked_mul(8)
            .ok_or_else(|| query_failure("query-capture-accounting"))?
            .min(HARD_MAX_QUERY_CAPTURES),
    })
}

pub(crate) struct QueryPackRegistry {
    packs: Vec<(GrammarFamily, QueryPack)>,
    typescript_tsx: QueryPack,
}

fn query_failure(code: &'static str) -> AdapterError {
    AdapterError::ProviderFailed {
        code: DiagnosticCode::new(code).expect("built-in query failure code is valid"),
    }
}

fn identity_scan_limit(
    limit: QueryLimit,
    maximum_matches: usize,
    maximum_captures: usize,
) -> AdapterError {
    let maximum = match limit {
        QueryLimit::Match => maximum_matches,
        QueryLimit::Capture => maximum_captures,
        QueryLimit::CursorMatch => usize::try_from(QUERY_CURSOR_MATCH_LIMIT).unwrap_or(usize::MAX),
        QueryLimit::Fact => 0,
    };
    AdapterError::Sink(SinkError::StreamLimit {
        resource: ResourceKind::Records,
        observed: maximum.saturating_add(1),
        limit: maximum,
    })
}

fn push_candidate_fallible(
    candidates: &mut Vec<QueryCandidate>,
    candidate: QueryCandidate,
    maximum: usize,
) -> Result<(), AdapterError> {
    if candidates.len() >= maximum {
        return Err(AdapterError::Sink(SinkError::StreamLimit {
            resource: ResourceKind::Records,
            observed: candidates.len().saturating_add(1),
            limit: maximum,
        }));
    }
    if candidates.len() == candidates.capacity() {
        let remaining = maximum.saturating_sub(candidates.len());
        candidates
            .try_reserve_exact(remaining.min(4096))
            .map_err(|_| AdapterError::Sink(SinkError::AllocationFailed))?;
    }
    candidates.push(candidate);
    Ok(())
}

fn candidate_for_capture(
    family: GrammarFamily,
    capture: QueryCapture<'_>,
    role: StructuralRole,
    source: &[u8],
) -> Result<QueryCandidate, AdapterError> {
    if family == GrammarFamily::Yaml {
        return yaml::candidate(capture.node, role);
    }
    if family == GrammarFamily::Html
        && role == StructuralRole::Signature
        && capture.node.kind() == "tag_name"
    {
        let owner = capture
            .node
            .parent()
            .and_then(|tag| tag.parent())
            .filter(|node| node.kind() == "element")
            .ok_or_else(|| query_failure("query-html-context-owner"))?;
        let name = capture
            .node
            .utf8_text(source)
            .map_err(|_| query_failure("query-html-context-name"))?;
        let syntax = if name.eq_ignore_ascii_case("noscript") {
            "html.scripting_context"
        } else if name.eq_ignore_ascii_case("svg") || name.eq_ignore_ascii_case("math") {
            "html.foreign_context"
        } else {
            return Err(query_failure("query-html-context-kind"));
        };
        return Ok(QueryCandidate {
            start: owner.start_byte(),
            end: owner.end_byte(),
            role,
            syntax,
            required: false,
            native_depth: 0,
        });
    }
    // These roles identify reviewed grammar fields rather than the many
    // concrete node kinds accepted by a grammar's shared node rules.
    let syntax = match role {
        StructuralRole::Scope | StructuralRole::Signature
            if family == GrammarFamily::Scala
                && capture.node.kind() == "given_definition"
                && capture.node.child_by_field_name("name").is_none() =>
        {
            "scala.anonymous_given"
        }
        StructuralRole::Declaration if family == GrammarFamily::Scala => {
            scala::declaration_syntax(capture.node, source)
                .ok_or_else(|| query_failure("query-scala-declaration-kind"))?
        }
        StructuralRole::Declaration
            if family == GrammarFamily::Solidity
                && capture.node.kind() == "state_variable_declaration" =>
        {
            let mut cursor = capture.node.walk();
            if capture
                .node
                .children(&mut cursor)
                .any(|child| child.kind() == "constant")
            {
                "solidity.constant"
            } else {
                "solidity.field"
            }
        }
        StructuralRole::Declaration if family == GrammarFamily::R => {
            r::declaration(capture.node, source)
                .ok_or_else(|| query_failure("query-r-declaration-kind"))?
                .syntax
        }
        StructuralRole::Signature if family == GrammarFamily::R => "r.function_header",
        StructuralRole::Scope if family == GrammarFamily::Toml => {
            if capture
                .node
                .parent()
                .is_some_and(|node| node.kind() == "array")
            {
                "toml.array_element"
            } else {
                canonical_syntax(family, capture.node.kind())
                    .ok_or_else(|| query_failure("query-toml-scope-kind"))?
            }
        }
        StructuralRole::Scope if family == GrammarFamily::Json => {
            match capture.node.parent().map(|node| node.kind()) {
                Some("array") => "json.array_element",
                Some("document") => "json.document_value",
                _ => canonical_syntax(family, capture.node.kind())
                    .ok_or_else(|| query_failure("query-json-scope-kind"))?,
            }
        }
        StructuralRole::ScopeTrait if family == GrammarFamily::Swift => "swift.extension_target",
        StructuralRole::ScopeType if family == GrammarFamily::Swift => "swift.extension_header",
        StructuralRole::ScopeType if family == GrammarFamily::Css => "css.context_header",
        _ if family == GrammarFamily::Swift && capture.node.kind() == "class_declaration" => {
            match capture
                .node
                .child_by_field_name("declaration_kind")
                .map(|node| node.kind())
            {
                Some("class") => "swift.class",
                Some("struct") => "swift.struct",
                Some("actor") => "swift.actor",
                Some("enum") => "swift.enum",
                Some("extension") => "swift.extension",
                _ => return Err(query_failure("query-swift-type-kind")),
            }
        }
        StructuralRole::Declaration if family == GrammarFamily::Swift => {
            match capture.node.kind() {
                "pattern" => {
                    let owner = capture.node.parent().and_then(|node| node.parent());
                    if owner.is_some_and(|node| {
                        matches!(
                            node.kind(),
                            "class_body" | "enum_class_body" | "protocol_body"
                        )
                    }) {
                        "swift.property"
                    } else {
                        "swift.variable"
                    }
                }
                "simple_identifier" => {
                    if capture
                        .node
                        .next_named_sibling()
                        .is_some_and(|node| node.kind() == "enum_type_parameters")
                    {
                        "swift.constructor"
                    } else {
                        "swift.constant"
                    }
                }
                kind => canonical_syntax(family, kind)
                    .ok_or_else(|| query_failure("query-swift-declaration-kind"))?,
            }
        }
        StructuralRole::Declaration if family == GrammarFamily::Lua => {
            lua_declaration_syntax(capture.node, source)
                .ok_or_else(|| query_failure("query-lua-declaration-kind"))?
        }
        StructuralRole::Declaration if family == GrammarFamily::Ruby => match capture.node.kind() {
            "assignment" => match capture
                .node
                .child_by_field_name("left")
                .map(|node| node.kind())
            {
                Some("constant" | "scope_resolution") => "ruby.constant",
                Some("instance_variable" | "class_variable") => "ruby.field",
                _ => "ruby.variable",
            },
            "identifier" => "ruby.parameter",
            kind => canonical_syntax(family, kind)
                .ok_or_else(|| query_failure("query-ruby-declaration-kind"))?,
        },
        StructuralRole::Declaration
            if family == GrammarFamily::C && is_c_function_prototype(capture.node) =>
        {
            "c.function"
        }
        StructuralRole::ScopeTrait => "rust.impl_trait",
        StructuralRole::ScopeType => "rust.impl_type",
        StructuralRole::TestAttribute => match family {
            GrammarFamily::Rust => "rust.test_attribute",
            GrammarFamily::Java => "java.test_attribute",
            GrammarFamily::Cpp => "cpp.test_attribute",
            _ => return Err(query_failure("query-test-attribute-family")),
        },
        StructuralRole::ScopedCall => "rust.scoped_call",
        StructuralRole::CallName => match family {
            GrammarFamily::C => "c.call_name",
            GrammarFamily::Cpp => "cpp.call_name",
            GrammarFamily::CSharp => "csharp.call_name",
            GrammarFamily::Go => "go.call_name",
            GrammarFamily::Java => "java.call_name",
            GrammarFamily::Php => "php.call_name",
            GrammarFamily::Lua => "lua.call_name",
            GrammarFamily::Ruby => "ruby.call_name",
            GrammarFamily::Bash => "bash.call_name",
            GrammarFamily::R => "r.call_name",
            GrammarFamily::Solidity => "solidity.call_name",
            GrammarFamily::Scala => "scala.call_name",
            _ => return Err(query_failure("query-call-name-family")),
        },
        StructuralRole::Call => match family {
            GrammarFamily::Rust => "rust.call",
            GrammarFamily::Python => "python.call",
            GrammarFamily::JavaScript => "javascript.call",
            GrammarFamily::Java => "java.call",
            GrammarFamily::Go => "go.call",
            GrammarFamily::TypeScript => "typescript.call",
            GrammarFamily::C => "c.call",
            GrammarFamily::Cpp => "cpp.call",
            GrammarFamily::CSharp => "csharp.call",
            GrammarFamily::Kotlin => "kotlin.call",
            GrammarFamily::Php => "php.call",
            GrammarFamily::Lua => "lua.call",
            GrammarFamily::Ruby => "ruby.call",
            GrammarFamily::Swift => "swift.call",
            GrammarFamily::Bash => "bash.call",
            GrammarFamily::R => r::call_syntax(capture.node),
            GrammarFamily::Solidity => "solidity.call",
            GrammarFamily::Scala => "scala.call",
            GrammarFamily::Css => return Err(query_failure("query-css-call-kind")),
            GrammarFamily::Json => return Err(query_failure("query-json-call-kind")),
            GrammarFamily::Toml => return Err(query_failure("query-toml-call-kind")),
            GrammarFamily::Yaml => return Err(query_failure("query-yaml-call-kind")),
            GrammarFamily::Html => return Err(query_failure("query-html-call-kind")),
            GrammarFamily::Sql => return Err(query_failure("query-sql-call-kind")),
        },
        _ => canonical_syntax(family, capture.node.kind())
            .ok_or_else(|| query_failure("query-node-kind"))?,
    };
    let mut start = capture.node.start_byte();
    let mut end = capture.node.end_byte();
    if family == GrammarFamily::R && role == StructuralRole::Signature {
        let range = r::signature_range(capture.node, source)
            .ok_or_else(|| query_failure("query-r-signature-header"))?;
        start = range.start;
        end = range.end;
    }
    if family == GrammarFamily::Rust
        && role == StructuralRole::Signature
        && let Some(function) = capture.node.parent()
        && function.kind() == "function_item"
    {
        // Return types and where clauses belong to the signature; braces in
        // const expressions or comments are not the native function body.
        start = function.start_byte();
        end = function
            .child_by_field_name("body")
            .ok_or_else(|| query_failure("query-rust-signature-body"))?
            .start_byte();
        while end > start && source.get(end - 1).is_some_and(u8::is_ascii_whitespace) {
            end -= 1;
        }
    }
    if family == GrammarFamily::Sql
        && role == StructuralRole::Signature
        && capture.node.kind() == "create_function"
    {
        let range = sql::signature_range(capture.node, source)
            .ok_or_else(|| query_failure("query-sql-signature-header"))?;
        start = range.start;
        end = range.end;
    }
    if family == GrammarFamily::Css && role == StructuralRole::ScopeType {
        // Keep raw header bytes: whitespace may terminate a CSS escape, and
        // braces inside strings must not be mistaken for the parser's body.
        let mut cursor = capture.node.walk();
        end = capture
            .node
            .named_children(&mut cursor)
            .find(|child| child.kind() == "block")
            .ok_or_else(|| query_failure("query-css-context-body"))?
            .start_byte();
    }
    if matches!(
        family,
        GrammarFamily::Swift | GrammarFamily::Bash | GrammarFamily::Solidity | GrammarFamily::Scala
    ) && matches!(role, StructuralRole::Signature | StructuralRole::ScopeType)
    {
        if let Some(body) = capture.node.child_by_field_name("body") {
            end = body.start_byte();
        }
        // Bodies never participate in signatures or extension scope identity.
        while end > start && source.get(end - 1).is_some_and(u8::is_ascii_whitespace) {
            end -= 1;
        }
    }
    if family == GrammarFamily::Ruby && role == StructuralRole::Signature {
        let header_field = match capture.node.kind() {
            "class" => "superclass",
            "method" | "singleton_method" => "parameters",
            _ => return Err(query_failure("query-ruby-signature-kind")),
        };
        // Ruby permits parameterless methods without parentheses. Capture their
        // real headers, excluding bodies from compact signatures and identity.
        end = capture
            .node
            .child_by_field_name(header_field)
            .or_else(|| capture.node.child_by_field_name("name"))
            .ok_or_else(|| query_failure("query-ruby-signature-header"))?
            .end_byte();
    }
    if family == GrammarFamily::Ruby
        && role == StructuralRole::Definition
        && let Some(parent) = capture.node.parent()
        && parent.kind() == "singleton_method"
        && let Some(receiver) = parent.child_by_field_name("object")
    {
        // Singleton and instance methods with the same name are distinct bindings.
        // Keep the receiver in source-backed identity; dynamic receivers fail the
        // shared static-name boundary rather than aliasing an instance method.
        start = receiver.start_byte();
    }
    Ok(QueryCandidate {
        start,
        end,
        role,
        syntax,
        required: false,
        native_depth: 0,
    })
}

fn is_c_function_prototype(node: tree_sitter::Node<'_>) -> bool {
    if node.kind() != "declaration" {
        return false;
    }
    let mut declarator = node.child_by_field_name("declarator");
    while let Some(node) = declarator {
        match node.kind() {
            "pointer_declarator" => declarator = node.child_by_field_name("declarator"),
            // Outer pointers change the return type. Require a direct function
            // name so a parenthesized function-pointer variable is not promoted.
            "function_declarator" => {
                return node
                    .child_by_field_name("declarator")
                    .is_some_and(|name| name.kind() == "identifier");
            }
            _ => return false,
        }
    }
    false
}

fn lua_declaration_syntax(node: tree_sitter::Node<'_>, source: &[u8]) -> Option<&'static str> {
    Some(match node.kind() {
        "function_declaration" => {
            if node.child(0).is_some_and(|child| child.kind() == "local") {
                "lua.local_function"
            } else if node
                .child_by_field_name("name")
                .is_some_and(|name| name.kind() == "method_index_expression")
            {
                "lua.method"
            } else {
                "lua.function"
            }
        }
        "assignment_statement" => "lua.function",
        "field" => "lua.field_function",
        "identifier"
            if node
                .parent()
                .is_some_and(|parent| parent.kind() == "parameters") =>
        {
            "lua.parameter"
        }
        "identifier" => lua_binding_syntax(node, source),
        "variable_declaration" => {
            let binding = node
                .named_child(0)
                .and_then(|child| {
                    if child.kind() == "assignment_statement" {
                        child.named_child(0)
                    } else {
                        Some(child)
                    }
                })
                .and_then(|list| list.child_by_field_name("name"));
            binding.map_or("lua.variable", |binding| {
                lua_binding_syntax(binding, source)
            })
        }
        _ => return None,
    })
}

fn lua_nonlexical_identifier(node: tree_sitter::Node<'_>) -> bool {
    if node.kind() != "identifier" {
        return false;
    }
    node.parent().is_some_and(|parent| match parent.kind() {
        "attribute" | "label_statement" | "goto_statement" => true,
        "dot_index_expression" => parent.child_by_field_name("field") == Some(node),
        "method_index_expression" => parent.child_by_field_name("method") == Some(node),
        "field" => {
            parent.child_by_field_name("name") == Some(node)
                && parent.child(0).is_some_and(|child| child.kind() != "[")
        }
        _ => false,
    })
}

fn lua_binding_syntax(binding: tree_sitter::Node<'_>, source: &[u8]) -> &'static str {
    let constant = binding
        .next_named_sibling()
        .filter(|node| node.kind() == "attribute")
        .and_then(|attribute| attribute.named_child(0))
        .and_then(|name| source.get(name.byte_range()))
        .is_some_and(|name| name == b"const");
    if constant {
        "lua.constant"
    } else {
        "lua.variable"
    }
}

const fn supports_terminal_call_name(family: GrammarFamily) -> bool {
    matches!(
        family,
        GrammarFamily::C
            | GrammarFamily::Cpp
            | GrammarFamily::CSharp
            | GrammarFamily::Go
            | GrammarFamily::Java
            | GrammarFamily::Php
            | GrammarFamily::Lua
            | GrammarFamily::Ruby
            | GrammarFamily::Bash
            | GrammarFamily::R
            | GrammarFamily::Solidity
            | GrammarFamily::Scala
    )
}

const fn supports_test_attribute(family: GrammarFamily) -> bool {
    matches!(family, GrammarFamily::Cpp | GrammarFamily::Java)
}

fn canonical_syntax(family: GrammarFamily, native: &str) -> Option<&'static str> {
    match (family, native) {
        (GrammarFamily::Scala, native) => scala::canonical_syntax(native),
        (GrammarFamily::Solidity, "source_file") => Some("solidity.file"),
        (GrammarFamily::Solidity, "contract_declaration" | "library_declaration") => {
            Some("solidity.class")
        }
        (GrammarFamily::Solidity, "interface_declaration") => Some("solidity.interface"),
        (GrammarFamily::Solidity, "struct_declaration") => Some("solidity.struct"),
        (GrammarFamily::Solidity, "enum_declaration") => Some("solidity.enum"),
        (GrammarFamily::Solidity, "enum_value") => Some("solidity.enum_value"),
        (GrammarFamily::Solidity, "user_defined_type_definition") => Some("solidity.type"),
        (GrammarFamily::Solidity, "function_definition" | "fallback_receive_definition") => {
            Some("solidity.function")
        }
        (GrammarFamily::Solidity, "constructor_definition") => Some("solidity.constructor"),
        (GrammarFamily::Solidity, "modifier_definition") => Some("solidity.modifier"),
        (GrammarFamily::Solidity, "event_definition") => Some("solidity.event"),
        (GrammarFamily::Solidity, "error_declaration") => Some("solidity.error"),
        (GrammarFamily::Solidity, "state_variable_declaration" | "struct_member") => {
            Some("solidity.field")
        }
        (GrammarFamily::Solidity, "constant_variable_declaration") => Some("solidity.constant"),
        (GrammarFamily::Solidity, "variable_declaration") => Some("solidity.variable"),
        (GrammarFamily::Solidity, "parameter" | "event_parameter" | "error_parameter") => {
            Some("solidity.parameter")
        }
        (GrammarFamily::Solidity, "identifier" | "constructor" | "fallback" | "receive") => {
            Some("solidity.identifier")
        }
        (GrammarFamily::Solidity, "user_defined_type") => Some("solidity.type_name"),
        (GrammarFamily::Solidity, "import_directive") => Some("solidity.import"),
        (GrammarFamily::Solidity, "function_body" | "block_statement") => Some("solidity.block"),
        (GrammarFamily::Solidity, "assembly_statement") => Some("solidity.assembly"),
        (GrammarFamily::Solidity, "for_statement") => Some("solidity.for"),
        (GrammarFamily::Solidity, "comment") => Some("solidity.comment"),
        (GrammarFamily::Solidity, "string") => Some("solidity.string"),
        (GrammarFamily::Sql, "program") => Some("sql.file"),
        (GrammarFamily::R, "program") => Some("r.file"),
        (GrammarFamily::R, "function_definition") => Some("r.function"),
        (GrammarFamily::R, "parameter") => Some("r.parameter"),
        (GrammarFamily::R, "identifier" | "dots" | "dot_dot_i") => Some("r.identifier"),
        (GrammarFamily::R, "namespace_operator") => Some("r.namespace_name"),
        (GrammarFamily::R, "extract_operator") => Some("r.member_name"),
        (GrammarFamily::R, "string") => Some("r.string"),
        (GrammarFamily::R, "comment") => Some("r.comment"),
        (GrammarFamily::Sql, "create_table") => Some("sql.table"),
        (GrammarFamily::Sql, "create_view") => Some("sql.view"),
        (GrammarFamily::Sql, "create_materialized_view") => Some("sql.materialized_view"),
        (GrammarFamily::Sql, "create_index") => Some("sql.index"),
        (GrammarFamily::Sql, "create_schema") => Some("sql.schema"),
        (GrammarFamily::Sql, "create_database") => Some("sql.database"),
        (GrammarFamily::Sql, "create_role") => Some("sql.role"),
        (GrammarFamily::Sql, "create_sequence") => Some("sql.sequence"),
        (GrammarFamily::Sql, "create_extension") => Some("sql.extension"),
        (GrammarFamily::Sql, "create_trigger") => Some("sql.trigger"),
        (GrammarFamily::Sql, "create_type") => Some("sql.type"),
        (GrammarFamily::Sql, "create_function") => Some("sql.function"),
        (GrammarFamily::Sql, "function_arguments") => Some("sql.arguments"),
        (GrammarFamily::Sql, "function_body") => Some("sql.body"),
        (GrammarFamily::Sql, "function_argument") => Some("sql.parameter"),
        (GrammarFamily::Sql, "column_definition") => Some("sql.column"),
        (GrammarFamily::Sql, "object_reference" | "identifier") => Some("sql.identifier"),
        (GrammarFamily::Sql, "literal") => Some("sql.literal"),
        (GrammarFamily::Sql, "comment" | "marginalia") => Some("sql.comment"),
        (GrammarFamily::Html, "document") => Some("html.file"),
        (GrammarFamily::Html, "element" | "script_element" | "style_element") => {
            Some("html.element")
        }
        (GrammarFamily::Html, "tag_name") => Some("html.tag_name"),
        (GrammarFamily::Html, "attribute") => Some("html.attribute"),
        (GrammarFamily::Html, "attribute_name") => Some("html.attribute_name"),
        (GrammarFamily::Html, "raw_text") => Some("html.embedded_text"),
        (GrammarFamily::Html, "erroneous_end_tag") => Some("html.unmatched_end_tag"),
        (GrammarFamily::Html, "attribute_value" | "text" | "entity") => Some("html.text"),
        (GrammarFamily::Html, "comment") => Some("html.comment"),
        (GrammarFamily::Toml, "document") => Some("toml.file"),
        (GrammarFamily::Toml, "table") => Some("toml.table"),
        (GrammarFamily::Toml, "table_array_element") => Some("toml.table_array_element"),
        (GrammarFamily::Toml, "pair") => Some("toml.property"),
        (GrammarFamily::Toml, "array") => Some("toml.array"),
        (GrammarFamily::Toml, "inline_table") => Some("toml.inline_table"),
        (GrammarFamily::Toml, "bare_key" | "quoted_key" | "dotted_key") => Some("toml.key"),
        (GrammarFamily::Toml, "string") => Some("toml.string"),
        (GrammarFamily::Toml, "comment") => Some("toml.comment"),
        (GrammarFamily::Json, "document") => Some("json.file"),
        (GrammarFamily::Json, "pair") => Some("json.property"),
        (GrammarFamily::Json, "object") => Some("json.object"),
        (GrammarFamily::Json, "array") => Some("json.array"),
        (GrammarFamily::Json, "string") => Some("json.string"),
        (GrammarFamily::Json, "comment") => Some("json.comment"),
        (GrammarFamily::Bash, "program") => Some("bash.file"),
        (GrammarFamily::Bash, "function_definition") => Some("bash.function"),
        (GrammarFamily::Bash, "variable_assignment") => Some("bash.variable"),
        (GrammarFamily::Bash, "variable_name") => Some("bash.variable"),
        (GrammarFamily::Bash, "special_variable_name" | "word") => Some("bash.identifier"),
        (GrammarFamily::Bash, "compound_statement" | "do_group") => Some("bash.block"),
        (GrammarFamily::Bash, "subshell") => Some("bash.subshell"),
        (GrammarFamily::Bash, "comment") => Some("bash.comment"),
        (GrammarFamily::Bash, "string" | "raw_string" | "heredoc_body") => Some("bash.string"),
        (GrammarFamily::Css, "stylesheet") => Some("css.file"),
        (GrammarFamily::Css, "rule_set") => Some("css.style_rule"),
        (GrammarFamily::Css, "selectors") => Some("css.selectors"),
        (GrammarFamily::Css, "keyframes_statement") => Some("css.keyframes"),
        (GrammarFamily::Css, "keyframes_name" | "property_name") => Some("css.identifier"),
        (GrammarFamily::Css, "declaration") => Some("css.property"),
        (GrammarFamily::Css, "import_statement") => Some("css.import"),
        (GrammarFamily::Css, "media_statement") => Some("css.media"),
        (GrammarFamily::Css, "supports_statement") => Some("css.supports"),
        (GrammarFamily::Css, "scope_statement") => Some("css.scope"),
        (GrammarFamily::Css, "at_rule") => Some("css.at_rule"),
        (GrammarFamily::Css, "keyframe_block") => Some("css.keyframe_step"),
        (GrammarFamily::Css, "block" | "keyframe_block_list") => Some("css.block"),
        (GrammarFamily::Css, "comment" | "js_comment") => Some("css.comment"),
        (GrammarFamily::Css, "string_value") => Some("css.string"),
        (GrammarFamily::Swift, "source_file") => Some("swift.file"),
        (GrammarFamily::Swift, "class_declaration") => Some("swift.type_scope"),
        (GrammarFamily::Swift, "protocol_declaration") => Some("swift.protocol"),
        (GrammarFamily::Swift, "function_declaration" | "protocol_function_declaration") => {
            Some("swift.function")
        }
        (GrammarFamily::Swift, "init_declaration") => Some("swift.constructor"),
        (GrammarFamily::Swift, "deinit_declaration") => Some("swift.method"),
        (GrammarFamily::Swift, "pattern") => Some("swift.binding"),
        (GrammarFamily::Swift, "parameter") => Some("swift.parameter"),
        (GrammarFamily::Swift, "typealias_declaration") => Some("swift.type_alias"),
        (GrammarFamily::Swift, "simple_identifier" | "type_identifier" | "init" | "deinit") => {
            Some("swift.identifier")
        }
        (GrammarFamily::Swift, "import_declaration") => Some("swift.import"),
        (
            GrammarFamily::Swift,
            "class_body" | "enum_class_body" | "protocol_body" | "function_body" | "statements"
            | "lambda_literal",
        ) => Some("swift.block"),
        (GrammarFamily::Swift, "comment" | "multiline_comment") => Some("swift.comment"),
        (
            GrammarFamily::Swift,
            "line_string_literal" | "multi_line_string_literal" | "raw_string_literal",
        ) => Some("swift.string"),
        (GrammarFamily::Ruby, "program") => Some("ruby.file"),
        (GrammarFamily::Ruby, "class") => Some("ruby.class"),
        (GrammarFamily::Ruby, "module") => Some("ruby.namespace"),
        (GrammarFamily::Ruby, "method" | "singleton_method") => Some("ruby.method"),
        (GrammarFamily::Ruby, "singleton_class") => Some("ruby.singleton_class"),
        (GrammarFamily::Ruby, "body_statement" | "block" | "do_block") => Some("ruby.block"),
        (GrammarFamily::Ruby, "method_parameters" | "block_parameters") => Some("ruby.parameters"),
        (
            GrammarFamily::Ruby,
            "identifier" | "constant" | "instance_variable" | "class_variable" | "global_variable"
            | "operator" | "setter",
        ) => Some("ruby.identifier"),
        (GrammarFamily::Ruby, "scope_resolution") => Some("ruby.qualified_identifier"),
        (GrammarFamily::Ruby, "assignment") => Some("ruby.variable"),
        (GrammarFamily::Ruby, "comment") => Some("ruby.comment"),
        (GrammarFamily::Ruby, "string" | "heredoc_body" | "simple_symbol" | "delimited_symbol") => {
            Some("ruby.string")
        }
        (GrammarFamily::Lua, "chunk") => Some("lua.file"),
        (GrammarFamily::Lua, "block") => Some("lua.block"),
        (GrammarFamily::Lua, "for_statement") => Some("lua.for"),
        (GrammarFamily::Lua, "repeat_statement") => Some("lua.repeat"),
        (GrammarFamily::Lua, "function_declaration" | "function_definition") => {
            Some("lua.function")
        }
        (GrammarFamily::Lua, "parameters") => Some("lua.parameters"),
        (GrammarFamily::Lua, "variable_declaration") => Some("lua.local_binding"),
        (GrammarFamily::Lua, "assignment_statement") => Some("lua.variable"),
        (GrammarFamily::Lua, "field") => Some("lua.field"),
        (GrammarFamily::Lua, "identifier") => Some("lua.identifier"),
        (GrammarFamily::Lua, "dot_index_expression" | "method_index_expression") => {
            Some("lua.qualified_name")
        }
        (GrammarFamily::Lua, "comment") => Some("lua.comment"),
        (GrammarFamily::Lua, "string") => Some("lua.string"),
        (GrammarFamily::Rust, "source_file") => Some("rust.file"),
        (GrammarFamily::Rust, "mod_item") => Some("rust.module"),
        (GrammarFamily::Rust, "function_item") => Some("rust.function"),
        (GrammarFamily::Rust, "struct_item") => Some("rust.struct"),
        (GrammarFamily::Rust, "enum_item") => Some("rust.enum"),
        (GrammarFamily::Rust, "trait_item") => Some("rust.trait"),
        (GrammarFamily::Rust, "type_item") => Some("rust.type"),
        (GrammarFamily::Rust, "const_item") => Some("rust.const"),
        (GrammarFamily::Rust, "static_item") => Some("rust.static"),
        (GrammarFamily::Rust, "use_declaration") => Some("rust.use"),
        (GrammarFamily::Rust, "parameters") => Some("rust.parameters"),
        (GrammarFamily::Rust, "impl_item") => Some("rust.impl"),
        (GrammarFamily::Rust, "block") => Some("rust.block"),
        (GrammarFamily::Rust, "identifier") => Some("rust.identifier"),
        (GrammarFamily::Rust, "type_identifier") => Some("rust.type_identifier"),
        (GrammarFamily::Rust, "line_comment") => Some("rust.line_comment"),
        (GrammarFamily::Rust, "block_comment") => Some("rust.block_comment"),
        (GrammarFamily::Rust, "string_literal") => Some("rust.string"),
        (GrammarFamily::Python, "module") => Some("python.module"),
        (GrammarFamily::Python, "function_definition") => Some("python.function"),
        (GrammarFamily::Python, "class_definition") => Some("python.class"),
        (GrammarFamily::Python, "import_statement") => Some("python.import"),
        (GrammarFamily::Python, "import_from_statement") => Some("python.import_from"),
        (GrammarFamily::Python, "parameters") => Some("python.parameters"),
        (GrammarFamily::Python, "block") => Some("python.block"),
        (GrammarFamily::Python, "identifier") => Some("python.identifier"),
        (GrammarFamily::Python, "comment") => Some("python.comment"),
        (GrammarFamily::Python, "string") => Some("python.string"),
        (GrammarFamily::JavaScript, "program") => Some("javascript.program"),
        (GrammarFamily::JavaScript, "function_declaration") => Some("javascript.function"),
        (GrammarFamily::JavaScript, "class_declaration") => Some("javascript.class"),
        (GrammarFamily::JavaScript, "method_definition") => Some("javascript.method"),
        (GrammarFamily::JavaScript, "variable_declarator") => Some("javascript.variable"),
        (GrammarFamily::JavaScript, "import_statement") => Some("javascript.import"),
        (GrammarFamily::JavaScript, "formal_parameters") => Some("javascript.parameters"),
        (GrammarFamily::JavaScript, "statement_block") => Some("javascript.block"),
        (GrammarFamily::JavaScript, "identifier") => Some("javascript.identifier"),
        (GrammarFamily::JavaScript, "type_identifier") => Some("javascript.identifier"),
        (GrammarFamily::JavaScript, "property_identifier") => {
            Some("javascript.property_identifier")
        }
        (GrammarFamily::JavaScript, "comment") => Some("javascript.comment"),
        (GrammarFamily::JavaScript, "string") => Some("javascript.string"),
        (GrammarFamily::JavaScript, "template_string") => Some("javascript.template"),
        (GrammarFamily::Java, "program") => Some("java.program"),
        (GrammarFamily::Java, "package_declaration") => Some("java.package"),
        (GrammarFamily::Java, "module_declaration") => Some("java.module"),
        (GrammarFamily::Java, "class_declaration") => Some("java.class"),
        (GrammarFamily::Java, "interface_declaration") => Some("java.interface"),
        (GrammarFamily::Java, "annotation_type_declaration") => Some("java.annotation"),
        (GrammarFamily::Java, "annotation_type_element_declaration") => {
            Some("java.annotation_element")
        }
        (GrammarFamily::Java, "enum_declaration") => Some("java.enum"),
        (GrammarFamily::Java, "record_declaration") => Some("java.record"),
        (GrammarFamily::Java, "method_declaration") => Some("java.method"),
        (GrammarFamily::Java, "constructor_declaration") => Some("java.constructor"),
        (GrammarFamily::Java, "field_declaration") => Some("java.field"),
        (GrammarFamily::Java, "local_variable_declaration") => Some("java.local_variable"),
        (GrammarFamily::Java, "variable_declarator") => Some("java.variable"),
        (GrammarFamily::Java, "import_declaration") => Some("java.import"),
        (GrammarFamily::Java, "formal_parameters") => Some("java.parameters"),
        (GrammarFamily::Java, "(") => Some("java.parameters"),
        (GrammarFamily::Java, "block") => Some("java.block"),
        (GrammarFamily::Java, "identifier") => Some("java.identifier"),
        (GrammarFamily::Java, "scoped_identifier") => Some("java.qualified_identifier"),
        (GrammarFamily::Java, "line_comment") => Some("java.line_comment"),
        (GrammarFamily::Java, "block_comment") => Some("java.block_comment"),
        (GrammarFamily::Java, "string_literal") => Some("java.string"),
        (GrammarFamily::Go, "source_file") => Some("go.file"),
        (GrammarFamily::Go, "package_clause") => Some("go.package"),
        (GrammarFamily::Go, "function_declaration") => Some("go.function"),
        (GrammarFamily::Go, "method_declaration") => Some("go.method"),
        (GrammarFamily::Go, "type_spec") => Some("go.type"),
        (GrammarFamily::Go, "var_spec") => Some("go.variable"),
        (GrammarFamily::Go, "const_spec") => Some("go.constant"),
        (GrammarFamily::Go, "import_declaration") => Some("go.import"),
        (GrammarFamily::Go, "parameter_list") => Some("go.parameters"),
        (GrammarFamily::Go, "block") => Some("go.block"),
        (GrammarFamily::Go, "identifier") => Some("go.identifier"),
        (GrammarFamily::Go, "field_identifier") => Some("go.field_identifier"),
        (GrammarFamily::Go, "type_identifier") => Some("go.type_identifier"),
        (GrammarFamily::Go, "package_identifier") => Some("go.package_identifier"),
        (GrammarFamily::Go, "comment") => Some("go.comment"),
        (GrammarFamily::Go, "interpreted_string_literal") => Some("go.string"),
        (GrammarFamily::Go, "raw_string_literal") => Some("go.raw_string"),
        (GrammarFamily::TypeScript, "program") => Some("typescript.program"),
        (GrammarFamily::TypeScript, "function_declaration") => Some("typescript.function"),
        (GrammarFamily::TypeScript, "function_signature") => Some("typescript.function_signature"),
        (GrammarFamily::TypeScript, "class_declaration") => Some("typescript.class"),
        (GrammarFamily::TypeScript, "abstract_class_declaration") => {
            Some("typescript.abstract_class")
        }
        (GrammarFamily::TypeScript, "interface_declaration") => Some("typescript.interface"),
        (GrammarFamily::TypeScript, "type_alias_declaration") => Some("typescript.type_alias"),
        (GrammarFamily::TypeScript, "enum_declaration") => Some("typescript.enum"),
        (GrammarFamily::TypeScript, "method_definition") => Some("typescript.method"),
        (GrammarFamily::TypeScript, "method_signature") => Some("typescript.method_signature"),
        (GrammarFamily::TypeScript, "abstract_method_signature") => {
            Some("typescript.abstract_method")
        }
        (GrammarFamily::TypeScript, "variable_declarator") => Some("typescript.variable"),
        (GrammarFamily::TypeScript, "import_statement") => Some("typescript.import"),
        (GrammarFamily::TypeScript, "formal_parameters") => Some("typescript.parameters"),
        (GrammarFamily::TypeScript, "statement_block") => Some("typescript.block"),
        (GrammarFamily::TypeScript, "identifier") => Some("typescript.identifier"),
        (GrammarFamily::TypeScript, "type_identifier") => Some("typescript.type_identifier"),
        (GrammarFamily::TypeScript, "property_identifier") => {
            Some("typescript.property_identifier")
        }
        (GrammarFamily::TypeScript, "comment") => Some("typescript.comment"),
        (GrammarFamily::TypeScript, "string") => Some("typescript.string"),
        (GrammarFamily::TypeScript, "template_string") => Some("typescript.template"),
        (GrammarFamily::C, "translation_unit") => Some("c.file"),
        (GrammarFamily::C, "preproc_include") => Some("c.include"),
        (GrammarFamily::C, "function_definition") => Some("c.function"),
        (GrammarFamily::C, "declaration") => Some("c.declaration"),
        (GrammarFamily::C, "struct_specifier") => Some("c.struct"),
        (GrammarFamily::C, "union_specifier") => Some("c.union"),
        (GrammarFamily::C, "enum_specifier") => Some("c.enum"),
        (GrammarFamily::C, "type_definition") => Some("c.type"),
        (GrammarFamily::C, "preproc_def") => Some("c.macro"),
        (GrammarFamily::C, "preproc_function_def") => Some("c.function_macro"),
        (GrammarFamily::C, "parameter_list") => Some("c.parameters"),
        (GrammarFamily::C, "compound_statement") => Some("c.block"),
        (GrammarFamily::C, "identifier") => Some("c.identifier"),
        (GrammarFamily::C, "field_identifier") => Some("c.field_identifier"),
        (GrammarFamily::C, "type_identifier") => Some("c.type_identifier"),
        (GrammarFamily::C, "comment") => Some("c.comment"),
        (GrammarFamily::C, "string_literal") => Some("c.string"),
        (GrammarFamily::C, "concatenated_string") => Some("c.concatenated_string"),
        (GrammarFamily::Cpp, "translation_unit") => Some("cpp.file"),
        (GrammarFamily::Cpp, "namespace_definition") => Some("cpp.namespace"),
        (GrammarFamily::Cpp, "function_definition") => Some("cpp.function"),
        (GrammarFamily::Cpp, "declaration") => Some("cpp.declaration"),
        (GrammarFamily::Cpp, "field_declaration") => Some("cpp.function"),
        (GrammarFamily::Cpp, "class_specifier") => Some("cpp.class"),
        (GrammarFamily::Cpp, "struct_specifier") => Some("cpp.struct"),
        (GrammarFamily::Cpp, "union_specifier") => Some("cpp.union"),
        (GrammarFamily::Cpp, "enum_specifier") => Some("cpp.enum"),
        (GrammarFamily::Cpp, "type_definition") => Some("cpp.type"),
        (GrammarFamily::Cpp, "template_declaration") => Some("cpp.template"),
        (GrammarFamily::Cpp, "preproc_include") => Some("cpp.include"),
        (GrammarFamily::Cpp, "parameter_list") => Some("cpp.parameters"),
        (GrammarFamily::Cpp, "template_parameter_list") => Some("cpp.template_parameters"),
        (GrammarFamily::Cpp, "compound_statement") => Some("cpp.block"),
        (GrammarFamily::Cpp, "declaration_list") => Some("cpp.declaration_list"),
        (GrammarFamily::Cpp, "identifier") => Some("cpp.identifier"),
        (GrammarFamily::Cpp, "field_identifier") => Some("cpp.field_identifier"),
        (GrammarFamily::Cpp, "type_identifier") => Some("cpp.type_identifier"),
        (GrammarFamily::Cpp, "namespace_identifier") => Some("cpp.namespace_identifier"),
        (GrammarFamily::Cpp, "comment") => Some("cpp.comment"),
        (GrammarFamily::Cpp, "string_literal") => Some("cpp.string"),
        (GrammarFamily::Cpp, "raw_string_literal") => Some("cpp.raw_string"),
        (GrammarFamily::Cpp, "concatenated_string") => Some("cpp.concatenated_string"),
        (GrammarFamily::CSharp, "compilation_unit") => Some("csharp.file"),
        (GrammarFamily::CSharp, "namespace_declaration") => Some("csharp.namespace"),
        (GrammarFamily::CSharp, "file_scoped_namespace_declaration") => {
            Some("csharp.file_namespace")
        }
        (GrammarFamily::CSharp, "class_declaration") => Some("csharp.class"),
        (GrammarFamily::CSharp, "interface_declaration") => Some("csharp.interface"),
        (GrammarFamily::CSharp, "struct_declaration") => Some("csharp.struct"),
        (GrammarFamily::CSharp, "record_declaration") => Some("csharp.record"),
        (GrammarFamily::CSharp, "enum_declaration") => Some("csharp.enum"),
        (GrammarFamily::CSharp, "delegate_declaration") => Some("csharp.delegate"),
        (GrammarFamily::CSharp, "method_declaration") => Some("csharp.method"),
        (GrammarFamily::CSharp, "constructor_declaration") => Some("csharp.constructor"),
        (GrammarFamily::CSharp, "property_declaration") => Some("csharp.property"),
        (GrammarFamily::CSharp, "field_declaration") => Some("csharp.field"),
        (GrammarFamily::CSharp, "using_directive") => Some("csharp.using"),
        (GrammarFamily::CSharp, "parameter_list") => Some("csharp.parameters"),
        (GrammarFamily::CSharp, "type_parameter_list") => Some("csharp.type_parameters"),
        (GrammarFamily::CSharp, "block") => Some("csharp.block"),
        (GrammarFamily::CSharp, "identifier") => Some("csharp.identifier"),
        (GrammarFamily::CSharp, "comment") => Some("csharp.comment"),
        (GrammarFamily::CSharp, "string_literal") => Some("csharp.string"),
        (GrammarFamily::CSharp, "verbatim_string_literal") => Some("csharp.verbatim_string"),
        (GrammarFamily::CSharp, "raw_string_literal") => Some("csharp.raw_string"),
        (GrammarFamily::CSharp, "interpolated_string_expression") => {
            Some("csharp.interpolated_string")
        }
        (GrammarFamily::Kotlin, "source_file") => Some("kotlin.file"),
        (GrammarFamily::Kotlin, "package_header") => Some("kotlin.package"),
        (GrammarFamily::Kotlin, "class_declaration") => Some("kotlin.class"),
        (GrammarFamily::Kotlin, "object_declaration") => Some("kotlin.object"),
        (GrammarFamily::Kotlin, "function_declaration") => Some("kotlin.function"),
        (GrammarFamily::Kotlin, "property_declaration") => Some("kotlin.property"),
        (GrammarFamily::Kotlin, "import") => Some("kotlin.import"),
        (GrammarFamily::Kotlin, "function_value_parameters") => Some("kotlin.parameters"),
        (GrammarFamily::Kotlin, "type_parameters") => Some("kotlin.type_parameters"),
        (GrammarFamily::Kotlin, "class_parameters") => Some("kotlin.class_parameters"),
        (GrammarFamily::Kotlin, "block") => Some("kotlin.block"),
        (GrammarFamily::Kotlin, "class_body") => Some("kotlin.class_body"),
        (GrammarFamily::Kotlin, "identifier") => Some("kotlin.identifier"),
        (GrammarFamily::Kotlin, "qualified_identifier") => Some("kotlin.qualified_identifier"),
        (GrammarFamily::Kotlin, "line_comment") => Some("kotlin.line_comment"),
        (GrammarFamily::Kotlin, "block_comment") => Some("kotlin.block_comment"),
        (GrammarFamily::Kotlin, "string_literal") => Some("kotlin.string"),
        (GrammarFamily::Kotlin, "multiline_string_literal") => Some("kotlin.multiline_string"),
        (GrammarFamily::Php, "program") => Some("php.program"),
        (GrammarFamily::Php, "namespace_definition") => Some("php.namespace"),
        (GrammarFamily::Php, "class_declaration") => Some("php.class"),
        (GrammarFamily::Php, "interface_declaration") => Some("php.interface"),
        (GrammarFamily::Php, "trait_declaration") => Some("php.trait"),
        (GrammarFamily::Php, "enum_declaration") => Some("php.enum"),
        (GrammarFamily::Php, "function_definition") => Some("php.function"),
        (GrammarFamily::Php, "method_declaration") => Some("php.method"),
        (GrammarFamily::Php, "property_declaration") => Some("php.property"),
        (GrammarFamily::Php, "const_declaration") => Some("php.constant"),
        (GrammarFamily::Php, "namespace_use_declaration") => Some("php.namespace_use"),
        (GrammarFamily::Php, "include_expression") => Some("php.include"),
        (GrammarFamily::Php, "include_once_expression") => Some("php.include_once"),
        (GrammarFamily::Php, "require_expression") => Some("php.require"),
        (GrammarFamily::Php, "require_once_expression") => Some("php.require_once"),
        (GrammarFamily::Php, "formal_parameters") => Some("php.parameters"),
        (GrammarFamily::Php, "compound_statement") => Some("php.block"),
        (GrammarFamily::Php, "declaration_list") => Some("php.declaration_list"),
        (GrammarFamily::Php, "name") => Some("php.name"),
        (GrammarFamily::Php, "qualified_name") => Some("php.qualified_name"),
        (GrammarFamily::Php, "variable_name") => Some("php.variable_name"),
        (GrammarFamily::Php, "comment") => Some("php.comment"),
        (GrammarFamily::Php, "string") => Some("php.string"),
        (GrammarFamily::Php, "encapsed_string") => Some("php.encapsed_string"),
        (GrammarFamily::Php, "nowdoc_string") => Some("php.nowdoc_string"),
        _ => None,
    }
}

impl QueryPackRegistry {
    pub(crate) fn audited() -> Result<Self, GrammarFamily> {
        let mut packs = Vec::with_capacity(24);
        for (family, source) in [
            (GrammarFamily::Rust, include_str!("../queries/rust.scm")),
            (GrammarFamily::Python, include_str!("../queries/python.scm")),
            (
                GrammarFamily::JavaScript,
                include_str!("../queries/javascript.scm"),
            ),
            (GrammarFamily::Java, include_str!("../queries/java.scm")),
            (GrammarFamily::Go, include_str!("../queries/go.scm")),
            (
                GrammarFamily::TypeScript,
                include_str!("../queries/typescript.scm"),
            ),
            (GrammarFamily::C, include_str!("../queries/c.scm")),
            (GrammarFamily::Cpp, include_str!("../queries/cpp.scm")),
            (GrammarFamily::CSharp, include_str!("../queries/csharp.scm")),
            (GrammarFamily::Kotlin, include_str!("../queries/kotlin.scm")),
            (GrammarFamily::Php, include_str!("../queries/php.scm")),
            (GrammarFamily::Lua, include_str!("../queries/lua.scm")),
            (GrammarFamily::Ruby, include_str!("../queries/ruby.scm")),
            (GrammarFamily::Swift, include_str!("../queries/swift.scm")),
            (GrammarFamily::Css, include_str!("../queries/css.scm")),
            (GrammarFamily::Bash, include_str!("../queries/bash.scm")),
            (GrammarFamily::Json, include_str!("../queries/json.scm")),
            (GrammarFamily::Toml, include_str!("../queries/toml.scm")),
            (GrammarFamily::Yaml, include_str!("../queries/yaml.scm")),
            (GrammarFamily::Html, include_str!("../queries/html.scm")),
            (GrammarFamily::Sql, include_str!("../queries/sql.scm")),
            (GrammarFamily::R, include_str!("../queries/r.scm")),
            (GrammarFamily::Scala, include_str!("../queries/scala.scm")),
            (
                GrammarFamily::Solidity,
                include_str!("../queries/solidity.scm"),
            ),
        ] {
            packs.push((family, QueryPack::compile(family, source)?));
        }
        packs.sort_by_key(|(family, _)| *family);
        let typescript_tsx = QueryPack::compile_native(
            GrammarFamily::TypeScript,
            GrammarFamily::JavaScript,
            include_str!("../queries/typescript.scm"),
        )?;
        Ok(Self {
            packs,
            typescript_tsx,
        })
    }

    pub(crate) fn get(&self, family: GrammarFamily) -> Option<&QueryPack> {
        self.packs
            .binary_search_by_key(&family, |(registered, _)| *registered)
            .ok()
            .and_then(|index| self.packs.get(index))
            .map(|(_, pack)| pack)
    }

    pub(crate) const fn len(&self) -> usize {
        self.packs.len() + 1
    }

    pub(crate) fn get_for_source(&self, family: GrammarFamily, path: &str) -> Option<&QueryPack> {
        if native_family_for_source(family, path) != family {
            Some(&self.typescript_tsx)
        } else {
            self.get(family)
        }
    }

    pub(crate) fn pattern_count(&self) -> usize {
        self.packs
            .iter()
            .map(|(_, pack)| pack.identity_query.pattern_count())
            .sum::<usize>()
            + self.typescript_tsx.identity_query.pattern_count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reviewed_packs_compile_with_the_exact_closed_capture_contract() {
        let registry = QueryPackRegistry::audited().expect("reviewed packs compile");

        for family in [
            GrammarFamily::Rust,
            GrammarFamily::Python,
            GrammarFamily::JavaScript,
            GrammarFamily::Java,
            GrammarFamily::Go,
            GrammarFamily::TypeScript,
            GrammarFamily::C,
            GrammarFamily::Cpp,
            GrammarFamily::CSharp,
            GrammarFamily::Kotlin,
            GrammarFamily::Php,
            GrammarFamily::Lua,
            GrammarFamily::Ruby,
            GrammarFamily::Swift,
            GrammarFamily::Css,
            GrammarFamily::Bash,
            GrammarFamily::Json,
            GrammarFamily::Toml,
            GrammarFamily::Yaml,
            GrammarFamily::Html,
            GrammarFamily::Sql,
            GrammarFamily::R,
            GrammarFamily::Solidity,
            GrammarFamily::Scala,
        ] {
            let pack = registry.get(family).expect("family has a query pack");
            let mut names = pack.identity_query.capture_names().to_vec();
            names.sort_unstable();
            let mut expected = EXPECTED_CAPTURES.to_vec();
            if family == GrammarFamily::Yaml {
                expected.retain(|name| !matches!(*name, "call" | "import"));
            }
            if family == GrammarFamily::Html {
                expected.retain(|name| !matches!(*name, "call" | "import" | "reference" | "scope"));
            }
            if family == GrammarFamily::Sql {
                expected.retain(|name| !matches!(*name, "call" | "import" | "scope"));
            }
            if matches!(family, GrammarFamily::Json | GrammarFamily::Toml) {
                expected
                    .retain(|name| !matches!(*name, "call" | "reference" | "signature" | "import"));
            }
            if matches!(
                family,
                GrammarFamily::Lua | GrammarFamily::Ruby | GrammarFamily::Bash | GrammarFamily::R
            ) {
                expected.retain(|name| *name != "import");
            }
            if family == GrammarFamily::Css {
                expected.retain(|name| !matches!(*name, "call" | "reference" | "signature"));
                expected.push("scope_type");
            }
            if family == GrammarFamily::Rust {
                expected.extend(RUST_SPECIAL_CAPTURES);
                expected.sort_unstable();
            } else {
                if family == GrammarFamily::Swift {
                    expected.extend(["scope_trait", "scope_type"]);
                }
                if supports_terminal_call_name(family) {
                    expected.push(TERMINAL_CALL_NAME_CAPTURE);
                }
                if supports_test_attribute(family) {
                    expected.push("test_attribute");
                }
                expected.sort_unstable();
            }
            assert_eq!(names, expected);
            assert_eq!(pack.roles_by_capture.len(), expected.len());
        }
    }

    #[test]
    fn mandatory_identity_scan_preserves_demand_beyond_the_optional_budget() {
        let mut source = b"fn first() {}\n".to_vec();
        source.extend(std::iter::repeat_n(b"fn repeated() {}\n".as_slice(), 256).flatten());
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&language_for(GrammarFamily::Rust))
            .expect("Rust grammar loads");
        let tree = parser.parse(&source, None).expect("fixture parses");
        let registry = QueryPackRegistry::audited().expect("reviewed packs compile");
        let pack = registry
            .get(GrammarFamily::Rust)
            .expect("Rust query pack exists");

        let extraction = pack
            .extract(
                GrammarFamily::Rust,
                &tree,
                &source,
                10_000,
                1,
                &Cancellation::new(),
            )
            .expect("identity demand remains available for exact normalization");

        assert!(
            extraction
                .candidates
                .iter()
                .filter(|candidate| candidate.required)
                .count()
                > 1
        );
    }
}
