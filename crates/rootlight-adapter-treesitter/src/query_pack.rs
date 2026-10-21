//! Reviewed structural query packs for Rootlight's audited grammars.
//!
//! Native queries and capture indices stay private; runtime extraction sees
//! only the closed, parser-independent role mapping defined here.

use std::{cmp::Ordering, collections::BinaryHeap, ops::ControlFlow, sync::OnceLock};

use rootlight_adapter_sdk::{
    AdapterError, DiagnosticCode, ResourceKind, SinkError, SyntaxFactKind,
};
use rootlight_cancel::{Cancellation, Cancelled};
use tree_sitter::{
    CaptureQuantifier, Query, QueryCapture, QueryCursor, QueryCursorOptions, StreamingIterator,
};

use crate::{
    GrammarFamily,
    registry::{language_for, native_family_for_source},
};

mod ada;
mod apex;
mod assembly;
mod bash;
pub(crate) mod c;
mod c_calls;
mod c_formal_reads;
mod c_parameters;
mod c_variables;
mod capnp;
#[cfg(test)]
mod capture_step_tests;
mod chapel;
mod clojure;
mod cmake;
mod cobol;
mod common_lisp;
mod coq;
mod cpp_const_formals;
mod crystal;
mod csharp_formals;
mod cue;
mod d;
mod dart;
#[cfg(test)]
mod deferred_tests;
mod ecmascript;
mod eiffel;
mod elixir;
mod fish;
mod fortran;
mod fortran_calls;
mod fsharp;
mod gdscript;
mod glsl;
mod go_calls;
mod go_formal_reads;
mod go_parameters;
mod graphql;
mod groovy;
mod hare;
mod haskell;
mod haxe;
mod hcl;
mod hdl;
mod hlsl;
pub(crate) mod identity_refusal;
#[cfg(test)]
mod identity_refusal_tests;
mod php_this_calls;
pub use identity_refusal::NativeIdentityRefusalDetails;
use identity_refusal::{IdentityRefusal, RefusalOrigin};
mod coffeescript;
mod dockerfile;
mod idris;
mod java_local_receivers;
mod java_parameters;
mod json_schema;
mod jsx;
mod julia;
mod lean;
mod markdown;
mod matlab;
mod meson;
mod modelica;
mod mojo;
mod nim;
mod nix;
mod objective_c;
mod ocaml;
mod pascal;
mod perl;
mod plsql;
mod pony;
mod powershell;
mod powershell_formals;
mod protobuf;
mod pug;
mod python_parameters;
mod python_variables;
mod qml;
mod qsharp;
mod r;
mod racket;
mod ring;
mod rust;
mod sas;
mod scala;
mod scala_formals;
mod scheme;
mod sed;
mod sml_wat;
mod sql;
mod sql_admission;
mod thrift;
mod typescript_calls;
mod v;
pub(super) mod vba;
mod vba_calls;
mod vhdl;
mod wgsl;
mod wolfram;
mod xml;
mod yaml;
mod zig;

const QUERY_CURSOR_MATCH_LIMIT: u32 = 4096;
const HARD_MAX_QUERY_MATCHES: usize = 1_048_576;
const HARD_MAX_QUERY_CAPTURES: usize = 2_097_152;
pub(crate) const HARD_MAX_QUERY_FACTS: usize = 1_048_576;
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
    Expression,
    Import,
    Scope,
    ScopeTrait,
    ScopeType,
    Definition,
    DefinitionPart,
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
            "expression" => Some(Self::Expression),
            "import" => Some(Self::Import),
            "scope" => Some(Self::Scope),
            "scope_trait" => Some(Self::ScopeTrait),
            "scope_type" => Some(Self::ScopeType),
            "definition" => Some(Self::Definition),
            "definition_part" => Some(Self::DefinitionPart),
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
            Self::Signature
            | Self::Expression
            | Self::ScopeTrait
            | Self::ScopeType
            | Self::TestAttribute => SyntaxFactKind::Signature,
            Self::Import => SyntaxFactKind::Import,
            Self::Scope => SyntaxFactKind::Scope,
            Self::Definition
            | Self::DefinitionPart
            | Self::Call
            | Self::CallName
            | Self::ScopedCall
            | Self::Reference => SyntaxFactKind::Occurrence,
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
            Self::Expression => "expression",
            Self::Import => "import",
            Self::Scope => "scope",
            Self::ScopeTrait => "scope_trait",
            Self::ScopeType => "scope_type",
            Self::Definition => "definition",
            Self::DefinitionPart => "definition_part",
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
            Self::Declaration | Self::Definition | Self::DefinitionPart => 2,
            Self::Signature | Self::Scope | Self::ScopeTrait | Self::ScopeType => 3,
            Self::TestAttribute => 4,
            Self::Import => 5,
            Self::Documentation => 6,
            Self::Call | Self::ScopedCall => 7,
            // A retained terminal name is safe only when its containing call
            // survived the same bounded extraction.
            Self::CallName | Self::Reference | Self::Expression => 8,
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
                | Self::DefinitionPart
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

/// Private evidence of the intrinsic identity-query work ceiling, not a sink quota.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct IntrinsicIdentityMatchLimit {
    maximum: usize,
}

impl IntrinsicIdentityMatchLimit {
    pub(crate) const fn limit(self) -> usize {
        self.maximum
    }

    pub(crate) const fn observed(self) -> usize {
        self.maximum.saturating_add(1)
    }

    pub(crate) fn error(self) -> AdapterError {
        identity_scan_limit(QueryLimit::Match, self.maximum, 0)
    }

    pub(crate) fn matches_error(self, error: &AdapterError) -> bool {
        matches!(error, AdapterError::Sink(SinkError::StreamLimit {
            resource: ResourceKind::Records, observed, limit,
        }) if *observed == self.observed() && *limit == self.limit())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct QueryCandidate {
    pub(crate) start: usize,
    pub(crate) end: usize,
    pub(crate) role: StructuralRole,
    pub(crate) syntax: &'static str,
    pub(crate) required: bool,
    // Native nesting disambiguates containers with identical byte ranges.
    pub(crate) native_depth: usize,
}

impl QueryCandidate {
    pub(crate) fn retention_rank(self) -> (u8, usize, usize, StructuralRole, &'static str, usize) {
        // Argument shape is useful only with complete call context. Under bounded
        // extraction, retain actual reads before these supplementary prefixes.
        let rank = if self.role == StructuralRole::Expression
            && matches!(
                self.syntax,
                "groovy.argument_positional_prefix"
                    | "groovy.argument_named_prefix"
                    | "groovy.argument_spread_prefix"
                    | "groovy.argument_closure_prefix"
                    | "groovy.value_read"
                    | "groovy.value_write"
            ) {
            9
        } else {
            self.role.retention_rank()
        };
        (
            rank,
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
    normalize_calls: bool,
}

#[derive(Clone, Copy)]
struct QueryScanLimits {
    matches: usize,
    captures: usize,
    body_nodes: usize,
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
        if family == GrammarFamily::Idris {
            expected.retain(|name| !matches!(*name, "call" | "import" | "documentation"));
        }

        if family == GrammarFamily::Sed {
            expected.retain(|name| {
                matches!(
                    *name,
                    "root"
                        | "module"
                        | "declaration"
                        | "definition"
                        | "reference"
                        | "signature"
                        | "comment"
                        | "string"
                )
            });
        }
        if matches!(family, GrammarFamily::Wolfram | GrammarFamily::Lean) {
            expected.retain(|name| !matches!(*name, "call" | "import" | "documentation"));
        }
        if native_family == GrammarFamily::JavaScript
            && matches!(
                family,
                GrammarFamily::JavaScript | GrammarFamily::TypeScript
            )
        {
            expected.push("expression");
        }
        if family == GrammarFamily::Graphql {
            expected.retain(|name| !matches!(*name, "call" | "import" | "documentation"));
        }
        if family == GrammarFamily::Elixir {
            expected
                .retain(|name| !matches!(*name, "call" | "reference" | "import" | "documentation"));
        }
        if family == GrammarFamily::Capnp {
            expected.retain(|name| !matches!(*name, "call" | "import" | "documentation"));
        }
        if matches!(family, GrammarFamily::Scala | GrammarFamily::PowerShell) {
            expected.push("expression");
        }
        if family == GrammarFamily::Coq {
            expected.retain(|name| !matches!(*name, "call" | "import" | "scope" | "documentation"));
        }
        if matches!(family, GrammarFamily::Ocaml | GrammarFamily::OcamlInterface) {
            expected.retain(|name| *name != "import");
            if family == GrammarFamily::OcamlInterface {
                expected.retain(|name| !matches!(*name, "call" | "reference"));
            }
            expected.push("expression");
        }
        if matches!(family, GrammarFamily::CobolFixed | GrammarFamily::CobolFree) {
            expected.retain(|name| !matches!(*name, "call" | "module" | "documentation"));
        }
        if family == GrammarFamily::Perl {
            expected.retain(|name| !matches!(*name, "call" | "import"));
        }
        if family == GrammarFamily::Fortran {
            expected.retain(|name| *name != "documentation");
            expected.push("expression");
        }
        if matches!(
            family,
            GrammarFamily::PlSql
                | GrammarFamily::Sas
                | GrammarFamily::Pascal
                | GrammarFamily::Clojure
                | GrammarFamily::Racket
                | GrammarFamily::CommonLisp
                | GrammarFamily::Scheme
                | GrammarFamily::Glsl
                | GrammarFamily::Hlsl
        ) {
            expected.retain(|name| !matches!(*name, "call" | "import" | "documentation"));
            expected.push("expression");
        }
        if family == GrammarFamily::Apex {
            expected.retain(|name| *name != "import");
            expected.push("expression");
        }
        if family == GrammarFamily::Protobuf {
            expected.retain(|name| !matches!(*name, "call" | "documentation"));
            expected.push("expression");
        }

        if family == GrammarFamily::Thrift {
            expected.retain(|name| !matches!(*name, "call" | "documentation"));
        }

        if family == GrammarFamily::Wgsl {
            expected.retain(|name| {
                !matches!(
                    *name,
                    "call" | "comment" | "documentation" | "import" | "string"
                )
            });
            expected.push("expression");
        }
        if family == GrammarFamily::Assembly {
            expected
                .retain(|name| !matches!(*name, "call" | "import" | "documentation" | "reference"));
            expected.push("expression");
        }
        if matches!(
            family,
            GrammarFamily::FSharp | GrammarFamily::FSharpSignature
        ) {
            expected.retain(|name| {
                !matches!(
                    *name,
                    "call" | "comment" | "documentation" | "reference" | "string"
                )
            });
            if family == GrammarFamily::FSharp {
                expected.extend(["call", "expression", "reference"]);
                expected.sort_unstable();
            } else {
                expected.push("expression");
                expected.sort_unstable();
            }
        }
        if family == GrammarFamily::Matlab || family.is_classic_basic() {
            expected.retain(|name| !matches!(*name, "call" | "import" | "documentation"));
        }
        if family.is_classic_basic() {
            expected.push("expression");
        }
        if family == GrammarFamily::JsonSchema {
            expected
                .retain(|name| !matches!(*name, "call" | "import" | "comment" | "documentation"));
            expected.push("expression");
        }
        if family.is_json() || family == GrammarFamily::Toml {
            expected.retain(|name| !matches!(*name, "call" | "reference" | "signature" | "import"));
        }
        if family == GrammarFamily::Pug {
            expected.retain(|name| {
                !matches!(
                    *name,
                    "call" | "import" | "reference" | "scope" | "documentation"
                )
            });
        }
        if family == GrammarFamily::Yaml {
            expected.retain(|name| !matches!(*name, "call" | "import"));
        }
        if family == GrammarFamily::Xml {
            expected.retain(|name| !matches!(*name, "call" | "import" | "scope"));
        }
        if matches!(
            family,
            GrammarFamily::Eiffel | GrammarFamily::Verilog | GrammarFamily::SystemVerilog
        ) {
            expected.retain(|name| !matches!(*name, "import" | "scope" | "documentation"));
        }
        if matches!(
            family,
            GrammarFamily::CMake | GrammarFamily::Julia | GrammarFamily::V
        ) {
            expected.retain(|name| !matches!(*name, "scope" | "documentation"));
        }
        if family == GrammarFamily::Vhdl {
            expected.retain(|name| !matches!(*name, "call" | "import" | "scope" | "documentation"));
        }
        if matches!(family, GrammarFamily::Cue | GrammarFamily::Hcl) {
            expected.retain(|name| {
                !matches!(
                    *name,
                    "call" | "import" | "reference" | "documentation" | "signature"
                )
            });
            expected.push("expression");
        }
        if matches!(
            family,
            GrammarFamily::Pony | GrammarFamily::Crystal | GrammarFamily::Zig | GrammarFamily::D
        ) {
            expected
                .retain(|name| !matches!(*name, "call" | "import" | "reference" | "documentation"));
            expected.push("expression");
        }
        if family == GrammarFamily::Qml {
            expected.retain(|name| !matches!(*name, "reference" | "import" | "documentation"));
            expected.push("expression");
        }
        if matches!(
            family,
            GrammarFamily::Coffeescript
                | GrammarFamily::Gdscript
                | GrammarFamily::Chapel
                | GrammarFamily::Qsharp
                | GrammarFamily::StandardMl
                | GrammarFamily::Wat
                | GrammarFamily::Mojo
                | GrammarFamily::Haxe
                | GrammarFamily::Modelica
                | GrammarFamily::Ring
                | GrammarFamily::Hare
        ) {
            expected.retain(|name| !matches!(*name, "call" | "import" | "documentation"));
            expected.push("expression");
        }
        if family == GrammarFamily::Dockerfile {
            expected.retain(|name| {
                !matches!(
                    *name,
                    "call" | "import" | "documentation" | "module" | "scope" | "string"
                )
            });
        }
        if family == GrammarFamily::Meson {
            expected.retain(|name| {
                !matches!(
                    *name,
                    "call" | "import" | "documentation" | "module" | "scope"
                )
            });
        }
        if family == GrammarFamily::Wat {
            expected.retain(|name| *name != "module");
        }
        if family == GrammarFamily::Ada {
            expected.retain(|name| !matches!(*name, "call" | "import" | "documentation"));
            expected.push("expression");
        }
        if family == GrammarFamily::Fish {
            expected
                .retain(|name| !matches!(*name, "call" | "import" | "reference" | "documentation"));
            expected.push("expression");
        }
        if family == GrammarFamily::Nim {
            expected.retain(|name| !matches!(*name, "call" | "import" | "documentation"));
            expected.push("expression");
        }
        if family == GrammarFamily::Dtd {
            expected.retain(|name| {
                !matches!(
                    *name,
                    "call" | "import" | "scope" | "declaration" | "definition"
                )
            });
        }
        if matches!(family, GrammarFamily::Html | GrammarFamily::Astro) {
            expected.retain(|name| !matches!(*name, "call" | "import" | "reference" | "scope"));
        }
        if family == GrammarFamily::Markdown {
            expected.retain(|name| {
                !matches!(
                    *name,
                    "call" | "import" | "reference" | "scope" | "comment" | "documentation"
                )
            });
        }
        if family == GrammarFamily::Sql {
            expected.retain(|name| !matches!(*name, "call" | "import" | "scope"));
            expected.push("expression");
        }
        if matches!(
            family,
            GrammarFamily::Lua
                | GrammarFamily::Ruby
                | GrammarFamily::Bash
                | GrammarFamily::R
                | GrammarFamily::PowerShell
                | GrammarFamily::Nix
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
            if family == GrammarFamily::ObjectiveC {
                expected.extend(["definition_part", "scope_trait", "scope_type"]);
            }
            if family == GrammarFamily::Nix {
                expected.extend(["definition_part", "expression"]);
            }
            if matches!(family, GrammarFamily::Perl | GrammarFamily::Groovy) {
                expected.push("expression");
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
                    *quantifier != CaptureQuantifier::Zero
                        && (role.belongs_to_identity_closure()
                            // Source-certified references remain mandatory even when
                            // their native pattern has no declaration capture.
                            || (matches!(family, GrammarFamily::Dockerfile | GrammarFamily::Sed | GrammarFamily::Idris)
                                && *role == StructuralRole::Reference))
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
        self.extract_with_identity_limit(
            family,
            tree,
            source,
            max_nodes,
            max_facts,
            cancellation,
            None,
            None,
        )
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "the native query inputs and private failure witness are distinct"
    )]
    pub(crate) fn extract_with_identity_limit(
        &self,
        family: GrammarFamily,
        tree: &tree_sitter::Tree,
        source: &[u8],
        max_nodes: usize,
        max_facts: usize,
        cancellation: &Cancellation,
        identity_limit: Option<&mut Option<IntrinsicIdentityMatchLimit>>,
        identity_refusal: Option<&mut Option<IdentityRefusal>>,
    ) -> Result<QueryExtraction, AdapterError> {
        cancellation.check()?;
        let identity_limits = identity_scan_limits(max_nodes);
        let mut identity_candidates = self.extract_identity_with_limit(
            family,
            tree,
            source,
            max_nodes,
            cancellation,
            identity_limit,
            identity_refusal,
        )?;

        let budgeted_matches = max_facts.saturating_mul(QUERY_MATCHES_PER_RETAINED_FACT);
        let budgeted_captures = max_facts.saturating_mul(QUERY_CAPTURES_PER_RETAINED_FACT);
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
                normalize_calls: true,
            },
            QueryScanLimits {
                matches: optional_max_matches,
                captures: optional_max_captures,
                body_nodes: max_nodes,
            },
            |candidate| {
                let candidate = RetainedCandidate(candidate);
                if candidates.len() < max_facts {
                    if candidates.len() == candidates.capacity() {
                        candidates
                            .try_reserve(1)
                            .map_err(|_| AdapterError::Sink(SinkError::AllocationFailed))?;
                    }
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
        self.extract_identity_with_limit(family, tree, source, max_nodes, cancellation, None, None)
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "legacy and current native refusal witnesses remain distinct"
    )]
    fn extract_identity_with_limit(
        &self,
        family: GrammarFamily,
        tree: &tree_sitter::Tree,
        source: &[u8],
        max_nodes: usize,
        cancellation: &Cancellation,
        identity_limit: Option<&mut Option<IntrinsicIdentityMatchLimit>>,
        identity_refusal: Option<&mut Option<IdentityRefusal>>,
    ) -> Result<Vec<QueryCandidate>, AdapterError> {
        cancellation.check()?;
        let limits = identity_scan_limits(max_nodes);
        let mut candidates = Vec::new();
        let mut refusal = None;
        let mut candidate_refusal = None;
        let scan = self.scan_query_with_refusal(
            &self.identity_query,
            QueryInput {
                family,
                tree,
                source,
                cancellation,
                normalize_calls: false,
            },
            limits,
            |mut candidate| {
                if (family == GrammarFamily::Dockerfile
                    && candidate.required
                    && candidate.role == StructuralRole::Reference
                    && candidate.syntax == "dockerfile.copy_from")
                    || (family == GrammarFamily::Meson
                        && candidate.required
                        && candidate.role == StructuralRole::Reference
                        && candidate.syntax == "meson.adjacent_iterable")
                    || (family == GrammarFamily::Lean
                        && candidate.required
                        && candidate.role == StructuralRole::Reference
                        && candidate.syntax == "lean.formal_body")
                    || candidate.role.belongs_to_identity_closure()
                    || (family == GrammarFamily::Idris
                        && candidate.required
                        && candidate.role == StructuralRole::Reference
                        && matches!(
                            candidate.syntax,
                            "idris.own_body_global" | "idris.unproved_name"
                        ))
                    || (family == GrammarFamily::Sed
                        && candidate.required
                        && candidate.role == StructuralRole::Reference
                        && matches!(
                            candidate.syntax,
                            "sed.written_branch" | "sed.unproved_branch"
                        ))
                    || (family == GrammarFamily::Python
                        && candidate.required
                        && candidate.role == StructuralRole::Reference
                        && candidate.syntax == "python.module_read")
                    || (family == GrammarFamily::Glsl
                        && candidate.required
                        && candidate.role == StructuralRole::Reference
                        && matches!(
                            candidate.syntax,
                            "glsl.formal_value" | "glsl.direct_file_function"
                        ))
                    || (family == GrammarFamily::StandardMl
                        && candidate.required
                        && candidate.role == StructuralRole::Reference
                        && candidate.syntax == "sml.own_function_call")
                    || (family == GrammarFamily::Wolfram
                        && candidate.required
                        && candidate.role == StructuralRole::Reference
                        && candidate.syntax == "wolfram.formal_argument")
                    || (family == GrammarFamily::Qsharp
                        && candidate.required
                        && candidate.role == StructuralRole::Reference
                        && candidate.syntax == "qsharp.formal_argument")
                    || (family == GrammarFamily::Mojo
                        && candidate.required
                        && matches!(
                            candidate.role,
                            StructuralRole::Reference | StructuralRole::Call
                        )
                        && matches!(
                            candidate.syntax,
                            "mojo.self_field_read" | "mojo.self_method_call"
                        ))
                    || (family == GrammarFamily::Php
                        && candidate.required
                        && candidate.role == StructuralRole::Reference
                        && candidate.syntax == "php.this_method")
                    || (family == GrammarFamily::Go
                        && candidate.required
                        && candidate.role == StructuralRole::Reference
                        && matches!(
                            candidate.syntax,
                            "go.call_formal"
                                | "go.direct_file_function"
                                | "go.function_value"
                                | "go.package_function_candidate"
                        ))
                    || (family == GrammarFamily::Chapel
                        && candidate.required
                        && candidate.role == StructuralRole::Reference
                        && candidate.syntax == "chapel.formal_initializer")
                    || (family == GrammarFamily::Java
                        && candidate.required
                        && candidate.role == StructuralRole::Reference
                        && candidate.syntax == "java.local_receiver")
                    || (family == GrammarFamily::PowerShell
                        && candidate.required
                        && candidate.role == StructuralRole::Reference
                        && candidate.syntax == "powershell.formal_value")
                    || (family == GrammarFamily::CSharp
                        && candidate.required
                        && candidate.role == StructuralRole::Reference
                        && candidate.syntax == "csharp.formal_receiver")
                    || (family == GrammarFamily::Cpp
                        && candidate.required
                        && candidate.role == StructuralRole::Reference
                        && candidate.syntax == "cpp.const_formal_receiver")
                    || (family == GrammarFamily::Scala
                        && candidate.required
                        && candidate.role == StructuralRole::Reference
                        && candidate.syntax == "scala.ordinary_formal")
                    || (family == GrammarFamily::Ocaml
                        && candidate.required
                        && matches!(
                            candidate.role,
                            StructuralRole::Reference | StructuralRole::Call
                        )
                        && matches!(
                            candidate.syntax,
                            "ocaml.match_formal" | "ocaml.direct_file_call"
                        ))
                    || (family == GrammarFamily::C
                        && candidate.required
                        && candidate.role == StructuralRole::Reference
                        && matches!(candidate.syntax, "c.formal_value" | "c.written_variable"))
                    || (family == GrammarFamily::Hare
                        && candidate.required
                        && candidate.role == StructuralRole::Reference
                        && matches!(
                            candidate.syntax,
                            "hare.direct_file_function" | "hare.unresolved_call"
                        ))
                    || (family == GrammarFamily::Scheme
                        && candidate.required
                        && candidate.role == StructuralRole::Reference
                        && candidate.syntax == "scheme.direct_file_call")
                    || (family == GrammarFamily::Ring
                        && candidate.required
                        && candidate.role == StructuralRole::Reference
                        && candidate.syntax == "ring.parameter_read")
                    || (family == GrammarFamily::Coq
                        && candidate.required
                        && candidate.role == StructuralRole::Reference
                        && candidate.syntax == "coq.written_reference")
                    || (family == GrammarFamily::Haxe
                        && candidate.required
                        && candidate.role == StructuralRole::Reference
                        && candidate.syntax == "haxe.local_read")
                {
                    // Ordinary scopes become mandatory only when they contain a
                    // declaration; runtime marks those after deduplication.
                    // Data scalar siblings are needed to count array positions,
                    // even when they contain no named declaration themselves.
                    candidate.required = candidate.role != StructuralRole::Scope
                        || matches!(
                            family,
                            GrammarFamily::Json
                                | GrammarFamily::Jsonc
                                | GrammarFamily::Toml
                                | GrammarFamily::Yaml
                                | GrammarFamily::FSharp
                        );
                    if candidates.len() >= limits.captures {
                        let event = IdentityRefusal::candidates(candidates.len(), limits.captures);
                        candidate_refusal = Some(event);
                        return Err(event.error());
                    }
                    push_candidate_fallible(&mut candidates, candidate, limits.captures)?;
                }
                Ok(())
            },
            Some(&mut refusal),
        );
        let limit = match scan {
            Ok(limit) => limit,
            Err(error) => {
                if let Some(event) = candidate_refusal
                    && event.matches_error(&error)
                    && limits.captures == HARD_MAX_QUERY_CAPTURES
                    && let Some(witness) = identity_refusal
                {
                    *witness = Some(event);
                }
                return Err(error);
            }
        };
        if let Some(event) = refusal {
            if limits.captures == HARD_MAX_QUERY_CAPTURES
                && limits.body_nodes == usize::MAX
                && let Some(witness) = identity_refusal
            {
                *witness = Some(event);
            }
            return Err(event.error());
        }
        if let Some(limit) = limit {
            if limit == QueryLimit::Match
                && limits.matches == HARD_MAX_QUERY_MATCHES
                && let Some(witness) = identity_limit
            {
                // Only this native branch can certify the intrinsic ceiling.
                // Caller node quotas, captures and sink limits are not equivalent.
                *witness = Some(IntrinsicIdentityMatchLimit {
                    maximum: limits.matches,
                });
            }
            return Err(identity_scan_limit(limit, limits.matches, limits.captures));
        }
        Ok(candidates)
    }

    fn scan_query(
        &self,
        query: &Query,
        input: QueryInput<'_>,
        limits: QueryScanLimits,
        retain: impl FnMut(QueryCandidate) -> Result<(), AdapterError>,
    ) -> Result<Option<QueryLimit>, AdapterError> {
        self.scan_query_with_refusal(query, input, limits, retain, None)
    }

    fn scan_query_with_refusal(
        &self,
        query: &Query,
        input: QueryInput<'_>,
        limits: QueryScanLimits,
        mut retain: impl FnMut(QueryCandidate) -> Result<(), AdapterError>,
        witness: Option<&mut Option<IdentityRefusal>>,
    ) -> Result<Option<QueryLimit>, AdapterError> {
        let mut match_count = 0usize;
        let mut capture_count = 0usize;
        let mut native_matches = 0usize;
        let mut native_captures = 0usize;
        let mut refusal = None;
        let mut vba_calls = vba_calls::Calls::default();
        if input.normalize_calls && input.family.is_classic_basic() {
            let limit = vba_calls.discover(
                input,
                limits,
                &mut match_count,
                &mut capture_count,
                &mut retain,
            )?;
            if limit.is_some() {
                return Ok(limit);
            }
        }
        let scala_package_prefix_end = if input.family == GrammarFamily::Scala {
            scala::leading_package_end(input.tree.root_node(), input.cancellation)?
        } else {
            None
        };
        let groovy_package = if input.family == GrammarFamily::Groovy {
            groovy::leading_package(input.tree.root_node(), input.cancellation)?
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
        let mut limit = None;
        let c_dialect = match input.family {
            GrammarFamily::C => Some(c::PreprocessorDialect::C),
            GrammarFamily::Cpp => Some(c::PreprocessorDialect::Cpp),
            _ => None,
        };
        let c_included_ranges = c_dialect.map(|_| input.tree.included_ranges());
        let complete_file_input = matches!(input.tree.included_ranges().as_slice(), [range]
            if range.start_byte == 0 && range.end_byte >= input.source.len());
        let mut python_variables = python_variables::CaptureCache::new(
            input.source,
            limits.captures.min(limits.body_nodes),
            complete_file_input,
        );
        let mut python_parameter_reads = python_parameters::CaptureCache::new(
            input.source,
            limits.captures.min(limits.body_nodes),
            matches!(input.tree.included_ranges().as_slice(), [range]
                if range.start_byte == 0 && range.end_byte >= input.source.len()),
        );
        let mut php_this_calls = php_this_calls::CaptureCache::new(
            input.source,
            limits.captures.min(limits.body_nodes),
            complete_file_input,
        );
        let mut go_calls = go_calls::CaptureCache::new(
            input.source,
            limits.captures.min(limits.body_nodes),
            complete_file_input,
        );
        let mut go_formal_reads = go_formal_reads::CaptureCache::new(
            input.source,
            limits.captures.min(limits.body_nodes),
            matches!(input.tree.included_ranges().as_slice(), [range]
                if range.start_byte == 0 && range.end_byte >= input.source.len()),
        );
        let mut cpp_const_formals = cpp_const_formals::CaptureCache::new(
            input.source,
            limits.captures.min(limits.body_nodes),
            complete_file_input,
        );
        let mut powershell_formals = powershell_formals::CaptureCache::new(
            input.source,
            limits.captures.min(limits.body_nodes),
            complete_file_input,
        );
        let mut csharp_formals = csharp_formals::CaptureCache::new(
            input.source,
            limits.captures.min(limits.body_nodes),
            complete_file_input,
        );
        let mut scala_formals = scala_formals::CaptureCache::new(
            input.source,
            limits.captures.min(limits.body_nodes),
            matches!(input.tree.included_ranges().as_slice(), [range]
                if range.start_byte == 0 && range.end_byte >= input.source.len()),
        );
        let mut c_variables = c_variables::CaptureCache::new(
            input.source,
            limits.captures.min(limits.body_nodes),
            complete_file_input,
        );
        let mut c_formal_reads = c_formal_reads::CaptureCache::new(
            input.source,
            limits.captures.min(limits.body_nodes),
            matches!(input.tree.included_ranges().as_slice(), [range]
                if range.start_byte == 0 && range.end_byte >= input.source.len()),
        );
        let mut java_local_receivers = java_local_receivers::CaptureCache::new(
            input.source,
            limits.captures.min(limits.body_nodes),
            matches!(input.tree.included_ranges().as_slice(), [range]
                if range.start_byte == 0 && range.end_byte >= input.source.len()),
        );
        let mut java_parameter_reads = java_parameters::CaptureCache::new(
            input.source,
            limits.captures.min(limits.body_nodes),
            matches!(input.tree.included_ranges().as_slice(), [range]
                if range.start_byte == 0 && range.end_byte >= input.source.len()),
        );
        let mut objective_c_parameters = objective_c::TypeParameterCaptures::default();
        let mut groovy_members = groovy::MemberKinds::default();
        let mut groovy_callables = groovy::CallableKinds::default();
        let mut groovy_calls = groovy::Calls::default();
        // Identity and optional queries have separate scans; RHS read indexing
        // belongs only to the optional scan that also normalizes call evidence.
        let mut fsharp_captures =
            fsharp::CaptureCache::new(limits.captures, input.normalize_calls, limits.body_nodes);
        let mut plsql_captures = plsql::CaptureCache::new(limits.captures.min(limits.body_nodes));
        let mut sas_captures = sas::CaptureCache::new(limits.captures.min(limits.body_nodes));
        let mut apex_captures =
            apex::CaptureCache::new(limits.captures.min(limits.body_nodes), limits.body_nodes);
        let mut pascal_captures = pascal::CaptureCache::new(limits.captures.min(limits.body_nodes));
        let mut common_lisp_captures =
            common_lisp::CaptureCache::new(limits.captures.min(limits.body_nodes));
        let mut scheme_captures = scheme::CaptureCache::new(limits.captures.min(limits.body_nodes))
            .with_complete_source(matches!(input.tree.included_ranges().as_slice(), [range]
                if range.start_byte == 0 && range.end_byte >= input.source.len()));
        let mut assembly_captures =
            assembly::CaptureCache::new(limits.captures.min(limits.body_nodes));
        let mut glsl_captures = glsl::CaptureCache::new_with_source_frame(
            limits.captures.min(limits.body_nodes),
            matches!(input.tree.included_ranges().as_slice(), [range] if range.start_byte == 0 && range.end_byte >= input.source.len()),
        );
        let mut hlsl_captures = hlsl::CaptureCache::new(limits.captures.min(limits.body_nodes));
        let mut wgsl_captures = wgsl::CaptureCache::new(limits.captures.min(limits.body_nodes));
        let mut xml_captures = xml::CaptureCache::new(limits.captures.min(limits.body_nodes));
        let mut cmake_captures = cmake::CaptureCache::new(limits.captures.min(limits.body_nodes));
        let mut coq_captures = coq::CaptureCache::new(
            input.tree.root_node(),
            limits.captures.min(limits.body_nodes),
        )
        .with_complete_context(matches!(input.tree.included_ranges().as_slice(), [range]
            if range.start_byte == 0 && range.end_byte >= input.source.len()));
        let mut julia_captures = julia::CaptureCache::new(limits.captures.min(limits.body_nodes));
        let mut clojure_captures =
            clojure::CaptureCache::new(limits.captures.min(limits.body_nodes));
        let mut racket_captures = racket::CaptureCache::new(limits.captures.min(limits.body_nodes));
        let mut typescript_calls =
            typescript_calls::CaptureCache::new(input.source, limits.body_nodes);
        let mut c_calls = c_calls::CaptureCache::new(input.source, limits.body_nodes);
        let mut bash_reads = bash::CaptureCache::new(input.source, limits.body_nodes);
        let mut haxe_captures = haxe::CaptureCache::new(
            input.source,
            limits.body_nodes,
            matches!(input.tree.included_ranges().as_slice(), [range] if range.start_byte == 0 && range.end_byte >= input.source.len()),
        );
        let mut dockerfile_captures = dockerfile::CaptureCache::new(
            input.source,
            limits.body_nodes.min(limits.captures),
            complete_file_input,
        );
        let mut wolfram_captures = wolfram::CaptureCache::new(
            input.source,
            limits.body_nodes.min(limits.captures),
            complete_file_input,
        );
        let mut idris_captures = idris::CaptureCache::new(
            input.source,
            input.tree.root_node(),
            limits.body_nodes.min(limits.captures),
            complete_file_input,
        );
        let mut sed_captures = sed::CaptureCache::new(
            input.source,
            input.tree.root_node(),
            limits.body_nodes.min(limits.captures),
            complete_file_input,
        );
        let mut lean_captures = lean::CaptureCache::new(
            input.source,
            input.tree.root_node(),
            limits.body_nodes.min(limits.captures),
            complete_file_input,
        );
        let mut pug_captures = pug::CaptureCache::new(
            input.source,
            input.tree.root_node(),
            limits.body_nodes.min(limits.captures),
            complete_file_input,
        );
        let mut coffeescript_captures = coffeescript::CaptureCache::new(
            input.source,
            limits.body_nodes.min(limits.captures),
            complete_file_input,
        );
        let mut meson_captures = meson::CaptureCache::new(
            input.source,
            limits.body_nodes.min(limits.captures),
            complete_file_input,
        );
        let mut gdscript_captures = gdscript::CaptureCache::new(
            input.source,
            limits.body_nodes.min(limits.captures),
            matches!(input.tree.included_ranges().as_slice(), [range] if range.start_byte == 0 && range.end_byte >= input.source.len()),
        );
        let mut qml_captures = qml::CaptureCache::new(input.source, limits.body_nodes);
        let mut fortran_calls =
            fortran_calls::CaptureCache::new(input.source, limits.body_nodes.min(limits.captures));
        let mut hare_captures = hare::CaptureCache::new(
            input.source,
            limits.body_nodes.min(limits.captures),
            matches!(input.tree.included_ranges().as_slice(), [range] if range.start_byte == 0 && range.end_byte >= input.source.len()),
        );
        let mut ring_captures = ring::CaptureCache::new(
            input.source,
            limits.body_nodes.min(limits.captures),
            matches!(input.tree.included_ranges().as_slice(), [range]
                if range.start_byte == 0 && range.end_byte >= input.source.len()),
        );
        let mut chapel_captures = chapel::CaptureCache::new(
            input.source,
            limits.body_nodes.min(limits.captures),
            complete_file_input,
        );
        let mut sml_captures = sml_wat::CaptureCache::new(
            input.source,
            limits.body_nodes.min(limits.captures),
            matches!(input.tree.included_ranges().as_slice(), [range] if range.start_byte == 0 && range.end_byte >= input.source.len()),
            false,
        );
        let mut wat_captures = sml_wat::CaptureCache::new(
            input.source,
            limits.body_nodes.min(limits.captures),
            matches!(input.tree.included_ranges().as_slice(), [range] if range.start_byte == 0 && range.end_byte >= input.source.len()),
            true,
        );
        let mut qsharp_captures = qsharp::CaptureCache::new(
            input.source,
            limits.body_nodes.min(limits.captures),
            matches!(input.tree.included_ranges().as_slice(),[range] if range.start_byte == 0 && range.end_byte >= input.source.len()),
        );
        let mut mojo_captures = mojo::CaptureCache::new(
            input.source,
            limits.body_nodes.min(limits.captures),
            matches!(input.tree.included_ranges().as_slice(), [range] if range.start_byte == 0 && range.end_byte >= input.source.len()),
        );
        let mut modelica_captures = modelica::CaptureCache::new(input.source, limits.body_nodes);
        let mut json_schema_captures = json_schema::CaptureCache::new(
            input.source,
            limits.body_nodes,
            input.cancellation,
            matches!(input.tree.included_ranges().as_slice(), [range]
                if range.start_byte == 0 && range.end_byte >= input.source.len()),
        );
        let mut vhdl_captures = vhdl::CaptureCache::new(limits.captures.min(limits.body_nodes));
        let mut crystal_captures =
            crystal::CaptureCache::new(limits.captures.min(limits.body_nodes));
        let mut pony_captures = pony::CaptureCache::new(limits.captures.min(limits.body_nodes));
        let mut fish_captures = fish::CaptureCache::new(limits.captures.min(limits.body_nodes));
        let mut hdl_captures = hdl::CaptureCache::new(limits.captures.min(limits.body_nodes));
        let mut v_captures = v::CaptureCache::new(limits.captures.min(limits.body_nodes));
        let mut rust_captures = rust::CaptureCache::default();
        let mut eiffel_captures = eiffel::CaptureCache::new(limits.captures.min(limits.body_nodes));
        let mut ocaml_captures = ocaml::CaptureCache::new(limits.captures.min(limits.body_nodes))
            .with_complete_context(matches!(input.tree.included_ranges().as_slice(), [range]
                if range.start_byte == 0 && range.end_byte >= input.source.len()));
        // A full-extent root can still omit bytes between included ranges, while
        // standalone roots may exclude leading or trailing trivia from their span.
        let fsharp_complete_source = !input.normalize_calls
            || !matches!(
                input.family,
                GrammarFamily::FSharp | GrammarFamily::FSharpSignature
            )
            || matches!(input.tree.included_ranges().as_slice(), [range]
                if range.start_byte == 0 && range.end_byte >= input.source.len());

        let jsx_complete_source = matches!(
            input.family,
            GrammarFamily::JavaScript | GrammarFamily::TypeScript
        ) && matches!(input.tree.included_ranges().as_slice(), [range]
            if range.start_byte == 0 && range.end_byte >= input.source.len());

        'query: while let Some(query_match) = matches.next() {
            if match_count >= limits.matches {
                limit = Some(QueryLimit::Match);
                break;
            }
            match_count = match_count
                .checked_add(1)
                .ok_or_else(|| query_failure("query-match-accounting"))?;
            native_matches = native_matches
                .checked_add(1)
                .ok_or_else(|| query_failure("query-native-match-accounting"))?;
            for capture in query_match.captures {
                if capture_count >= limits.captures {
                    refusal = Some(IdentityRefusal::capture_slots(
                        capture_count,
                        limits.captures,
                    ));
                    limit = Some(QueryLimit::Capture);
                    break 'query;
                }
                capture_count = capture_count
                    .checked_add(1)
                    .ok_or_else(|| query_failure("query-capture-accounting"))?;
                native_captures = native_captures
                    .checked_add(1)
                    .ok_or_else(|| query_failure("query-native-capture-accounting"))?;
                let role = self
                    .role_for_capture(capture.index)
                    .ok_or_else(|| query_failure("query-capture-role"))?;
                if role == StructuralRole::Reference
                    && vba_calls.contains(capture.node.byte_range())
                {
                    continue;
                }
                if input.family == GrammarFamily::Lua
                    && role == StructuralRole::Reference
                    && lua_nonlexical_identifier(capture.node)
                {
                    // Member accesses retain their complete qualified capture;
                    // literal keys, labels and binding attributes are not lexical reads.
                    continue;
                }
                let mut capture = *capture;
                if input.family == GrammarFamily::Go
                    && go_parameters::is_parameter_capture(capture.node, role)
                {
                    if let Some(candidate) = go_parameters::candidate(
                        capture.node,
                        role,
                        input.source,
                        input.cancellation,
                    )? {
                        retain(candidate)?;
                    }
                    continue;
                }
                if input.family == GrammarFamily::C
                    && c_parameters::is_parameter_capture(capture.node, role)
                {
                    if let Some(candidate) =
                        c_parameters::candidate(capture.node, role, input.cancellation)?
                    {
                        retain(candidate)?;
                    }
                    continue;
                }
                if input.family == GrammarFamily::Clojure {
                    let candidate = clojure::candidate(
                        capture.node,
                        role,
                        input.source,
                        input.cancellation,
                        &mut clojure_captures,
                    )?;
                    if clojure_captures.exhausted() {
                        refusal = Some(IdentityRefusal::helper(RefusalOrigin::ClojureCaptures));
                        limit = Some(QueryLimit::Capture);
                        break 'query;
                    }
                    if let Some(candidate) = candidate {
                        retain(candidate)?;
                    }
                    continue;
                }
                if input.family == GrammarFamily::Racket {
                    let keep = racket::retain_capture(
                        capture.node,
                        role,
                        input.source,
                        input.cancellation,
                        &mut racket_captures,
                    )?;
                    if racket_captures.exhausted() {
                        refusal = Some(IdentityRefusal::helper(RefusalOrigin::RacketCaptures));
                        limit = Some(QueryLimit::Capture);
                        break 'query;
                    }
                    if keep {
                        retain(racket::candidate(
                            capture.node,
                            role,
                            input.source,
                            input.cancellation,
                            &mut racket_captures,
                        )?)?;
                    }
                    if racket_captures.exhausted() {
                        refusal = Some(IdentityRefusal::helper(RefusalOrigin::RacketCaptures));
                        limit = Some(QueryLimit::Capture);
                        break 'query;
                    }
                    continue;
                }
                if input.family == GrammarFamily::Ada {
                    if ada::retain_capture(capture.node, role, input.source, input.cancellation)? {
                        retain(ada::candidate(
                            capture.node,
                            role,
                            input.source,
                            input.cancellation,
                        )?)?;
                    }
                    continue;
                }
                if input.family == GrammarFamily::Zig {
                    if zig::retain_capture(capture.node, role, input.source, input.cancellation)? {
                        retain(zig::candidate(
                            capture.node,
                            role,
                            input.source,
                            input.cancellation,
                        )?)?;
                    }
                    continue;
                }
                if input.family == GrammarFamily::Hcl {
                    if hcl::retain_capture(capture.node, role, input.source, input.cancellation)? {
                        retain(hcl::candidate(
                            capture.node,
                            role,
                            input.source,
                            input.cancellation,
                        )?)?;
                    }
                    continue;
                }
                if input.family == GrammarFamily::JsonSchema {
                    let candidate = json_schema::candidate(
                        capture.node,
                        role,
                        input.source,
                        input.cancellation,
                        &mut json_schema_captures,
                    )?;
                    if json_schema_captures.exhausted() {
                        refusal = Some(IdentityRefusal::helper(RefusalOrigin::JsonSchemaCaptures));
                        limit = Some(QueryLimit::Capture);
                        break 'query;
                    }
                    if let Some(candidate) = candidate {
                        retain(candidate)?;
                    }
                    continue;
                }
                if input.family == GrammarFamily::Chapel {
                    let candidate = chapel::candidate(
                        capture.node,
                        role,
                        input.cancellation,
                        &mut chapel_captures,
                    )?;
                    if chapel_captures.exhausted() {
                        refusal = Some(IdentityRefusal::helper(RefusalOrigin::ChapelCaptures));
                        limit = Some(QueryLimit::Capture);
                        break 'query;
                    }
                    if let Some(candidate) = candidate {
                        retain(candidate)?;
                    }
                    continue;
                }
                if input.family == GrammarFamily::StandardMl {
                    let candidate = sml_wat::candidate(
                        capture.node,
                        role,
                        input.source,
                        input.cancellation,
                        &mut sml_captures,
                    )?;
                    if sml_captures.exhausted() {
                        refusal = Some(IdentityRefusal::helper(RefusalOrigin::SmlCaptures));
                        limit = Some(QueryLimit::Capture);
                        break 'query;
                    }
                    if let Some(candidate) = candidate {
                        retain(candidate)?;
                    }
                    continue;
                }
                if input.family == GrammarFamily::Wat {
                    let candidate = sml_wat::candidate(
                        capture.node,
                        role,
                        input.source,
                        input.cancellation,
                        &mut wat_captures,
                    )?;
                    if wat_captures.exhausted() {
                        refusal = Some(IdentityRefusal::helper(RefusalOrigin::WatCaptures));
                        limit = Some(QueryLimit::Capture);
                        break 'query;
                    }
                    if let Some(candidate) = candidate {
                        retain(candidate)?;
                    }
                    continue;
                }
                if input.family == GrammarFamily::Dockerfile {
                    let candidate = dockerfile::candidate(
                        capture.node,
                        role,
                        input.cancellation,
                        &mut dockerfile_captures,
                    )?;
                    if dockerfile_captures.exhausted() {
                        refusal = Some(IdentityRefusal::helper(RefusalOrigin::DockerCaptures));
                        limit = Some(QueryLimit::Capture);
                        break 'query;
                    }
                    if let Some(candidate) = candidate {
                        retain(candidate)?;
                    }
                    continue;
                }
                if input.family == GrammarFamily::Idris {
                    let candidate = idris::candidate(
                        capture.node,
                        role,
                        input.cancellation,
                        &mut idris_captures,
                    )?;
                    if idris_captures.exhausted() {
                        refusal = Some(IdentityRefusal::helper(RefusalOrigin::IdrisCaptures));
                        limit = Some(QueryLimit::Capture);
                        break 'query;
                    }
                    if let Some(candidate) = candidate {
                        retain(candidate)?;
                    }
                    continue;
                }
                if input.family == GrammarFamily::Sed {
                    let candidate =
                        sed::candidate(capture.node, role, input.cancellation, &mut sed_captures)?;
                    if sed_captures.exhausted() {
                        refusal = Some(IdentityRefusal::helper(RefusalOrigin::SedCaptures));
                        limit = Some(QueryLimit::Capture);
                        break 'query;
                    }
                    if let Some(candidate) = candidate {
                        retain(candidate)?;
                    }
                    continue;
                }
                if input.family == GrammarFamily::Wolfram {
                    let candidate = wolfram::candidate(
                        capture.node,
                        role,
                        input.source,
                        input.cancellation,
                        &mut wolfram_captures,
                    )?;
                    if wolfram_captures.exhausted() {
                        refusal = Some(IdentityRefusal::helper(RefusalOrigin::WolframCaptures));
                        limit = Some(QueryLimit::Capture);
                        break 'query;
                    }
                    if let Some(candidate) = candidate {
                        retain(candidate)?;
                    }
                    continue;
                }
                if input.family == GrammarFamily::Lean {
                    let candidate = lean::candidate(
                        capture.node,
                        role,
                        input.cancellation,
                        &mut lean_captures,
                    )?;
                    if lean_captures.exhausted() {
                        refusal = Some(IdentityRefusal::helper(RefusalOrigin::LeanCaptures));
                        limit = Some(QueryLimit::Capture);
                        break 'query;
                    }
                    if let Some(candidate) = candidate {
                        retain(candidate)?;
                    }
                    continue;
                }
                if input.family == GrammarFamily::Pug {
                    let candidate =
                        pug::candidate(capture.node, role, input.cancellation, &mut pug_captures)?;
                    if pug_captures.exhausted() {
                        refusal = Some(IdentityRefusal::helper(RefusalOrigin::PugCaptures));
                        limit = Some(QueryLimit::Capture);
                        break 'query;
                    }
                    if let Some(candidate) = candidate {
                        retain(candidate)?;
                    }
                    continue;
                }
                if input.family == GrammarFamily::Coffeescript {
                    let candidate = coffeescript::candidate(
                        capture.node,
                        role,
                        input.source,
                        input.cancellation,
                        &mut coffeescript_captures,
                    )?;
                    if coffeescript_captures.exhausted() {
                        refusal =
                            Some(IdentityRefusal::helper(RefusalOrigin::CoffeescriptCaptures));
                        limit = Some(QueryLimit::Capture);
                        break 'query;
                    }
                    if let Some(candidate) = candidate {
                        retain(candidate)?;
                    }
                    continue;
                }
                if input.family == GrammarFamily::Meson {
                    let candidate = meson::candidate(
                        capture.node,
                        role,
                        input.cancellation,
                        &mut meson_captures,
                    )?;
                    if meson_captures.exhausted() {
                        refusal = Some(IdentityRefusal::helper(RefusalOrigin::MesonCaptures));
                        limit = Some(QueryLimit::Capture);
                        break 'query;
                    }
                    if let Some(candidate) = candidate {
                        retain(candidate)?;
                    }
                    continue;
                }
                if input.family == GrammarFamily::Gdscript {
                    let candidate = gdscript::candidate(
                        capture.node,
                        role,
                        input.source,
                        input.cancellation,
                        &mut gdscript_captures,
                    )?;
                    if gdscript_captures.exhausted() {
                        refusal = Some(IdentityRefusal::helper(RefusalOrigin::GdscriptCaptures));
                        limit = Some(QueryLimit::Capture);
                        break 'query;
                    }
                    if let Some(candidate) = candidate {
                        retain(candidate)?;
                    }
                    continue;
                }
                if input.family == GrammarFamily::Qsharp {
                    let candidate = qsharp::candidate(
                        capture.node,
                        role,
                        input.source,
                        input.cancellation,
                        &mut qsharp_captures,
                    )?;
                    if qsharp_captures.exhausted() {
                        refusal = Some(IdentityRefusal::helper(RefusalOrigin::QsharpCaptures));
                        limit = Some(QueryLimit::Capture);
                        break 'query;
                    }
                    if let Some(candidate) = candidate {
                        retain(candidate)?;
                    }
                    continue;
                }
                if input.family == GrammarFamily::Mojo {
                    let candidate = mojo::candidate(
                        capture.node,
                        role,
                        input.source,
                        input.cancellation,
                        &mut mojo_captures,
                    )?;
                    if mojo_captures.exhausted() {
                        refusal = Some(IdentityRefusal::helper(RefusalOrigin::MojoCaptures));
                        limit = Some(QueryLimit::Capture);
                        break 'query;
                    }
                    if let Some(candidate) = candidate {
                        retain(candidate)?;
                    }
                    continue;
                }
                if input.family == GrammarFamily::Haxe {
                    let candidate = haxe::candidate(
                        capture.node,
                        role,
                        input.source,
                        input.cancellation,
                        &mut haxe_captures,
                    )?;
                    if haxe_captures.exhausted() {
                        refusal = Some(IdentityRefusal::helper(RefusalOrigin::HaxeCaptures));
                        limit = Some(QueryLimit::Capture);
                        break 'query;
                    }
                    if let Some(candidate) = candidate {
                        retain(candidate)?;
                    }
                    continue;
                }
                if input.family == GrammarFamily::Qml {
                    let candidate = qml::candidate(
                        capture.node,
                        role,
                        input.source,
                        input.cancellation,
                        &mut qml_captures,
                    )?;
                    if qml_captures.exhausted() {
                        refusal = Some(IdentityRefusal::helper(RefusalOrigin::QmlCaptures));
                        limit = Some(QueryLimit::Capture);
                        break 'query;
                    }
                    if let Some(candidate) = candidate {
                        retain(candidate)?;
                    }
                    continue;
                }
                if input.family == GrammarFamily::Hare {
                    let candidate = hare::candidate(
                        capture.node,
                        role,
                        input.source,
                        input.cancellation,
                        &mut hare_captures,
                    )?;
                    if hare_captures.exhausted() {
                        refusal = Some(IdentityRefusal::helper(RefusalOrigin::HareCaptures));
                        limit = Some(QueryLimit::Capture);
                        break 'query;
                    }
                    if let Some(candidate) = candidate {
                        retain(candidate)?;
                    }
                    continue;
                }
                if input.family == GrammarFamily::Ring {
                    let candidate = ring::candidate(
                        capture.node,
                        role,
                        input.source,
                        input.cancellation,
                        &mut ring_captures,
                    )?;
                    if ring_captures.exhausted() {
                        refusal = Some(IdentityRefusal::helper(RefusalOrigin::RingCaptures));
                        limit = Some(QueryLimit::Capture);
                        break 'query;
                    }
                    if let Some(candidate) = candidate {
                        retain(candidate)?;
                    }
                    continue;
                }
                if input.family == GrammarFamily::Modelica {
                    let candidate = modelica::candidate(
                        capture.node,
                        role,
                        input.source,
                        input.cancellation,
                        &mut modelica_captures,
                    )?;
                    if modelica_captures.exhausted() {
                        refusal = Some(IdentityRefusal::helper(RefusalOrigin::ModelicaCaptures));
                        limit = Some(QueryLimit::Capture);
                        break 'query;
                    }
                    if let Some(candidate) = candidate {
                        retain(candidate)?;
                    }
                    continue;
                }
                if input.family == GrammarFamily::D {
                    if d::retain_capture(capture.node, role, input.source, input.cancellation)? {
                        retain(d::candidate(
                            capture.node,
                            role,
                            input.source,
                            input.cancellation,
                        )?)?;
                    }
                    continue;
                }
                if input.family == GrammarFamily::Cue {
                    if cue::retain_capture(capture.node, role, input.source, input.cancellation)? {
                        retain(cue::candidate(
                            capture.node,
                            role,
                            input.source,
                            input.cancellation,
                        )?)?;
                    }
                    continue;
                }
                if input.family == GrammarFamily::Crystal {
                    let candidate = crystal::candidate(
                        capture.node,
                        role,
                        input.source,
                        input.cancellation,
                        &mut crystal_captures,
                    )?;
                    if crystal_captures.exhausted() {
                        refusal = Some(IdentityRefusal::helper(RefusalOrigin::CrystalCaptures));
                        limit = Some(QueryLimit::Capture);
                        break 'query;
                    }
                    if let Some(candidate) = candidate {
                        retain(candidate)?;
                    }
                    continue;
                }
                if input.family == GrammarFamily::Pony {
                    let candidate = pony::candidate(
                        capture.node,
                        role,
                        input.source,
                        input.cancellation,
                        &mut pony_captures,
                    )?;
                    if pony_captures.exhausted() {
                        refusal = Some(IdentityRefusal::helper(RefusalOrigin::PonyCaptures));
                        limit = Some(QueryLimit::Capture);
                        break 'query;
                    }
                    if let Some(candidate) = candidate {
                        retain(candidate)?;
                    }
                    continue;
                }
                if input.family == GrammarFamily::Fish {
                    let candidate = fish::candidate(
                        capture.node,
                        role,
                        input.source,
                        input.cancellation,
                        &mut fish_captures,
                    )?;
                    if fish_captures.exhausted() {
                        refusal = Some(IdentityRefusal::helper(RefusalOrigin::FishCaptures));
                        limit = Some(QueryLimit::Capture);
                        break 'query;
                    }
                    if let Some(candidate) = candidate {
                        retain(candidate)?;
                    }
                    continue;
                }
                if input.family == GrammarFamily::Nim {
                    if nim::retain_capture(capture.node, role, input.source, input.cancellation)? {
                        retain(nim::candidate(
                            capture.node,
                            role,
                            input.source,
                            input.cancellation,
                        )?)?;
                    }
                    continue;
                }
                if input.family == GrammarFamily::Vhdl {
                    let candidate = vhdl::candidate(
                        capture.node,
                        role,
                        input.source,
                        input.cancellation,
                        &mut vhdl_captures,
                    )?;
                    if vhdl_captures.exhausted() {
                        refusal = Some(IdentityRefusal::helper(RefusalOrigin::VhdlCaptures));
                        limit = Some(QueryLimit::Capture);
                        break 'query;
                    }
                    if let Some(candidate) = candidate {
                        retain(candidate)?;
                    }
                    continue;
                }
                if input.family == GrammarFamily::Coq {
                    let candidate = coq::candidate(
                        capture.node,
                        role,
                        input.source,
                        input.cancellation,
                        &mut coq_captures,
                    )?;
                    if coq_captures.exhausted() {
                        refusal = Some(IdentityRefusal::helper(RefusalOrigin::CoqCaptures));
                        limit = Some(QueryLimit::Capture);
                        break 'query;
                    }
                    let reference = coq::required_reference_candidate(
                        capture.node,
                        role,
                        input.cancellation,
                        &coq_captures,
                    )?;
                    if let Some(candidate) = candidate {
                        retain(candidate)?;
                    }
                    if let Some(reference) = reference {
                        retain(reference)?;
                    }
                    continue;
                }
                if matches!(
                    input.family,
                    GrammarFamily::Ocaml | GrammarFamily::OcamlInterface
                ) {
                    let certificates = ocaml::binding_candidates(
                        input.family,
                        capture.node,
                        role,
                        input.source,
                        input.cancellation,
                        &mut ocaml_captures,
                    )?;
                    let specialized = certificates.iter().any(Option::is_some);
                    for certificate in certificates.into_iter().flatten() {
                        retain(certificate)?;
                    }
                    if specialized
                        && matches!(role, StructuralRole::Reference | StructuralRole::Call)
                    {
                        continue;
                    }
                    let candidate = ocaml::candidate(
                        input.family,
                        capture.node,
                        role,
                        input.source,
                        input.cancellation,
                        &mut ocaml_captures,
                    )?;
                    if ocaml_captures.exhausted() {
                        refusal = Some(IdentityRefusal::helper(RefusalOrigin::OcamlCaptures));
                        limit = Some(QueryLimit::Capture);
                        break 'query;
                    }
                    if let Some(candidate) = candidate {
                        retain(candidate)?;
                    }
                    continue;
                }
                if matches!(
                    input.family,
                    GrammarFamily::Verilog | GrammarFamily::SystemVerilog
                ) {
                    let candidate = hdl::candidate(
                        input.family,
                        capture.node,
                        role,
                        input.source,
                        input.cancellation,
                        &mut hdl_captures,
                    )?;
                    if hdl_captures.exhausted() {
                        refusal = Some(IdentityRefusal::helper(RefusalOrigin::HdlCaptures));
                        limit = Some(QueryLimit::Capture);
                        break 'query;
                    }
                    if let Some(candidate) = candidate {
                        retain(candidate)?;
                    }
                    continue;
                }
                if input.family == GrammarFamily::V {
                    let candidate = v::candidate(
                        capture.node,
                        role,
                        input.source,
                        input.cancellation,
                        &mut v_captures,
                    )?;
                    if v_captures.exhausted() {
                        refusal = Some(IdentityRefusal::helper(RefusalOrigin::VCaptures));
                        limit = Some(QueryLimit::Capture);
                        break 'query;
                    }
                    if let Some(candidate) = candidate {
                        retain(candidate)?;
                    }
                    continue;
                }
                if input.family == GrammarFamily::Eiffel {
                    let candidate = eiffel::candidate(
                        capture.node,
                        role,
                        input.source,
                        input.cancellation,
                        &mut eiffel_captures,
                    )?;
                    if eiffel_captures.exhausted() {
                        refusal = Some(IdentityRefusal::helper(RefusalOrigin::EiffelCaptures));
                        limit = Some(QueryLimit::Capture);
                        break 'query;
                    }
                    if let Some(candidate) = candidate {
                        retain(candidate)?;
                    }
                    continue;
                }
                if input.family == GrammarFamily::Julia {
                    let candidate = julia::candidate(
                        capture.node,
                        role,
                        input.source,
                        input.cancellation,
                        &mut julia_captures,
                    )?;
                    if julia_captures.exhausted() {
                        refusal = Some(IdentityRefusal::helper(RefusalOrigin::JuliaCaptures));
                        limit = Some(QueryLimit::Capture);
                        break 'query;
                    }
                    if let Some(candidate) = candidate {
                        retain(candidate)?;
                    }
                    continue;
                }
                if input.family == GrammarFamily::CMake {
                    let candidate = cmake::candidate(
                        capture.node,
                        role,
                        input.source,
                        input.cancellation,
                        &mut cmake_captures,
                    )?;
                    if cmake_captures.exhausted() {
                        refusal = Some(IdentityRefusal::helper(RefusalOrigin::CmakeCaptures));
                        limit = Some(QueryLimit::Capture);
                        break 'query;
                    }
                    if let Some(candidate) = candidate {
                        retain(candidate)?;
                    }
                    continue;
                }
                if matches!(input.family, GrammarFamily::Xml | GrammarFamily::Dtd) {
                    let candidate = xml::candidate(
                        input.family,
                        capture.node,
                        role,
                        input.source,
                        input.cancellation,
                        &mut xml_captures,
                    )?;
                    if xml_captures.exhausted() {
                        refusal = Some(IdentityRefusal::helper(RefusalOrigin::XmlCaptures));
                        limit = Some(QueryLimit::Capture);
                        break 'query;
                    }
                    if let Some(candidate) = candidate {
                        retain(candidate)?;
                    }
                    continue;
                }
                if input.family == GrammarFamily::Wgsl {
                    let admitted = wgsl::retain_capture(
                        capture.node,
                        role,
                        input.source,
                        input.cancellation,
                        &mut wgsl_captures,
                    )?;
                    if wgsl_captures.exhausted() {
                        refusal = Some(IdentityRefusal::helper(RefusalOrigin::WgslCaptures));
                        limit = Some(QueryLimit::Capture);
                        break 'query;
                    }
                    if !admitted {
                        continue;
                    }
                }
                if input.family == GrammarFamily::Hlsl {
                    let admitted = hlsl::retain_capture(
                        capture.node,
                        role,
                        input.source,
                        input.cancellation,
                        &mut hlsl_captures,
                    )?;
                    if hlsl_captures.exhausted() {
                        refusal = Some(IdentityRefusal::helper(RefusalOrigin::HlslCaptures));
                        limit = Some(QueryLimit::Capture);
                        break 'query;
                    }
                    if !admitted {
                        continue;
                    }
                }
                if input.family == GrammarFamily::Glsl {
                    let admitted = glsl::retain_capture(
                        capture.node,
                        role,
                        input.source,
                        input.cancellation,
                        &mut glsl_captures,
                    )?;
                    if glsl_captures.exhausted() {
                        refusal = Some(IdentityRefusal::helper(RefusalOrigin::GlslCaptures));
                        limit = Some(QueryLimit::Capture);
                        break 'query;
                    }
                    if !admitted {
                        continue;
                    }
                }
                if input.family == GrammarFamily::Assembly {
                    let admitted = assembly::retain_capture(
                        capture.node,
                        role,
                        input.source,
                        input.cancellation,
                        &mut assembly_captures,
                    )?;
                    if assembly_captures.exhausted() {
                        refusal = Some(IdentityRefusal::helper(RefusalOrigin::AssemblyCaptures));
                        limit = Some(QueryLimit::Capture);
                        break 'query;
                    }
                    if !admitted {
                        continue;
                    }
                }
                if input.family == GrammarFamily::CommonLisp {
                    let admitted = common_lisp::retain_capture(
                        capture.node,
                        role,
                        input.source,
                        input.cancellation,
                        &mut common_lisp_captures,
                    )?;
                    if common_lisp_captures.exhausted() {
                        refusal = Some(IdentityRefusal::helper(RefusalOrigin::CommonLispCaptures));
                        limit = Some(QueryLimit::Capture);
                        break 'query;
                    }
                    if !admitted {
                        continue;
                    }
                }
                if input.family == GrammarFamily::Scheme {
                    let admitted = scheme::retain_capture(
                        capture.node,
                        role,
                        input.source,
                        input.cancellation,
                        &mut scheme_captures,
                    )?;
                    if scheme_captures.exhausted() {
                        refusal = Some(IdentityRefusal::helper(RefusalOrigin::SchemeCaptures));
                        limit = Some(QueryLimit::Capture);
                        break 'query;
                    }
                    if !admitted {
                        continue;
                    }
                }
                if input.family == GrammarFamily::Pascal {
                    let admitted = pascal::retain_capture(
                        capture.node,
                        role,
                        input.source,
                        input.cancellation,
                        &mut pascal_captures,
                    )?;
                    if pascal_captures.exhausted() {
                        refusal = Some(IdentityRefusal::helper(RefusalOrigin::PascalCaptures));
                        limit = Some(QueryLimit::Capture);
                        break 'query;
                    }
                    if !admitted {
                        continue;
                    }
                }
                if input.family == GrammarFamily::Apex {
                    let admitted = apex::retain_capture(
                        capture.node,
                        role,
                        input.source,
                        input.cancellation,
                        &mut apex_captures,
                    )?;
                    if apex_captures.exhausted() {
                        refusal = Some(IdentityRefusal::helper(RefusalOrigin::ApexCaptures));
                        limit = Some(QueryLimit::Capture);
                        break 'query;
                    }
                    if !admitted {
                        continue;
                    }
                }
                if input.family == GrammarFamily::Sas {
                    let admitted = sas::retain_capture(
                        capture.node,
                        role,
                        input.source,
                        input.cancellation,
                        &mut sas_captures,
                    )?;
                    if sas_captures.exhausted() {
                        refusal = Some(IdentityRefusal::helper(RefusalOrigin::SasCaptures));
                        limit = Some(QueryLimit::Capture);
                        break 'query;
                    }
                    if !admitted {
                        continue;
                    }
                }
                if input.family == GrammarFamily::PlSql {
                    let admitted = plsql::retain_capture(
                        capture.node,
                        role,
                        input.source,
                        input.cancellation,
                        &mut plsql_captures,
                    )?;
                    if plsql_captures.exhausted() {
                        refusal = Some(IdentityRefusal::helper(RefusalOrigin::PlsqlCaptures));
                        limit = Some(QueryLimit::Capture);
                        break 'query;
                    }
                    if !admitted {
                        continue;
                    }
                }
                if input.family == GrammarFamily::Capnp
                    && !capnp::retain_capture(capture.node, role, input.source, input.cancellation)?
                {
                    continue;
                }
                if input.family == GrammarFamily::Fortran {
                    if fortran::retain_capture(
                        capture.node,
                        role,
                        input.source,
                        input.cancellation,
                    )? {
                        let mut candidate = fortran::candidate(
                            capture.node,
                            role,
                            input.source,
                            input.cancellation,
                        )?;
                        if role == StructuralRole::Call
                            && fortran_calls.direct_call(capture.node, input.cancellation)?
                        {
                            candidate.syntax = "fortran.direct_module_subroutine";
                        }
                        if fortran_calls.exhausted() {
                            return Err(fortran_calls.exhaustion_error());
                        }
                        retain(candidate)?;
                    }
                    continue;
                }
                if input.family == GrammarFamily::Groovy
                    && role == StructuralRole::Reference
                    && groovy::nonlexical_identifier(capture.node)
                {
                    continue;
                }
                if input.family == GrammarFamily::Perl
                    && !perl::retain_capture(capture.node, role, input.source, input.cancellation)?
                {
                    continue;
                }
                if input.family == GrammarFamily::Matlab
                    && !matlab::retain_capture(capture.node, role, input.cancellation)?
                {
                    continue;
                }
                if input.family == GrammarFamily::Haskell
                    && !haskell::retain_capture(capture.node, role, input.cancellation)?
                {
                    continue;
                }
                if matches!(
                    input.family,
                    GrammarFamily::FSharp | GrammarFamily::FSharpSignature
                ) {
                    if role == StructuralRole::Expression
                        && capture.node.kind() == "file"
                        && !fsharp_complete_source
                    {
                        continue;
                    }
                    let admitted = fsharp::retain_capture(
                        capture.node,
                        role,
                        input.source,
                        input.cancellation,
                        &mut fsharp_captures,
                    );
                    if fsharp_captures.exhausted() {
                        refusal = Some(IdentityRefusal::helper(RefusalOrigin::FsharpCaptures));
                        limit = Some(QueryLimit::Capture);
                        break 'query;
                    }
                    if !admitted? {
                        continue;
                    }
                }
                if matches!(
                    input.family,
                    GrammarFamily::CobolFixed | GrammarFamily::CobolFree
                ) && !cobol::retain_capture(
                    capture.node,
                    role,
                    input.source,
                    input.cancellation,
                )? {
                    continue;
                }
                if input.family.is_classic_basic()
                    && role == StructuralRole::Reference
                    && vba::is_pointer_fragment(capture.node, input.source, input.cancellation)?
                {
                    continue;
                }
                if input.family.is_classic_basic()
                    && role == StructuralRole::Expression
                    && capture.node.kind() == "assignment_statement"
                    && !vba::implicit_comparison_statement(
                        capture.node,
                        input.source,
                        input.cancellation,
                    )?
                {
                    continue;
                }
                if input.family.is_classic_basic()
                    && !vba::retain_capture(capture.node, role, input.source)
                {
                    continue;
                }
                if input.family.is_classic_basic()
                    && role == StructuralRole::Reference
                    && let Some(member) = vba::self_member_node(capture.node, input.source)
                {
                    capture.node = member;
                }
                if input.family.is_classic_basic()
                    && capture.node.kind() == "source_file"
                    && role == StructuralRole::Signature
                {
                    let Some(header) =
                        vba::class_header(capture.node, input.source, input.cancellation)?
                    else {
                        continue;
                    };
                    capture.node = header;
                }
                if input.family.is_classic_basic()
                    && capture.node.kind() == "source_file"
                    && matches!(role, StructuralRole::Definition | StructuralRole::Reference)
                {
                    let Some(name) =
                        vba::module_name_node(capture.node, input.source, input.cancellation)?
                    else {
                        continue;
                    };
                    let proven = input
                        .source
                        .get(name.byte_range())
                        .and_then(|text| std::str::from_utf8(text).ok())
                        .and_then(rootlight_adapter_sdk::vba_module_name)
                        .is_some();
                    if proven != (role == StructuralRole::Definition) {
                        continue;
                    }
                    capture.node = name;
                }
                if input.family == GrammarFamily::Nix {
                    if !nix::retain_capture(capture.node, role) {
                        continue;
                    }
                    if role == StructuralRole::Definition {
                        let Some(name) =
                            nix::definition_node(capture.node, input.source, input.cancellation)?
                        else {
                            continue;
                        };
                        capture.node = name;
                    }
                    if role == StructuralRole::Expression {
                        capture.node = nix::expression_node(capture.node, input.cancellation)?;
                    }
                }
                if input.family == GrammarFamily::ObjectiveC
                    && !objective_c_parameters.retain(capture.node, role, input.cancellation)?
                {
                    continue;
                }
                if input.family == GrammarFamily::ObjectiveC
                    && !objective_c::retain_capture(capture.node, role)
                {
                    continue;
                }
                if matches!(
                    input.family,
                    GrammarFamily::JavaScript | GrammarFamily::TypeScript
                ) && !jsx::retain_capture(
                    capture.node,
                    role,
                    input.source,
                    jsx_complete_source,
                    input.cancellation,
                )? {
                    continue;
                }
                if matches!(
                    input.family,
                    GrammarFamily::JavaScript | GrammarFamily::TypeScript
                ) && !ecmascript::retain_capture(capture.node, role, input.cancellation)?
                {
                    continue;
                }
                if input.family == GrammarFamily::PowerShell
                    && !powershell::retain_capture(capture.node, role)
                {
                    continue;
                }
                if input.family == GrammarFamily::Rust
                    && !rust_captures.retain(
                        capture.node,
                        role,
                        input.source,
                        input.cancellation,
                    )?
                {
                    continue;
                }
                if input.family == GrammarFamily::Dart && !dart::retain_capture(capture.node, role)
                {
                    continue;
                }
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
                    && !sql_admission::retain(capture.node, role, input.cancellation)?
                {
                    continue;
                }
                if input.family == GrammarFamily::Sql
                    && role == StructuralRole::Reference
                    && sql::is_declared_name(capture.node)
                {
                    continue;
                }
                if input.family == GrammarFamily::Sql && role == StructuralRole::Definition {
                    let Some(name) = sql::declaration_name(capture.node, input.cancellation)?
                    else {
                        continue;
                    };
                    capture.node = name;
                }
                let c_macro_end = if let Some(dialect) = c_dialect
                    && matches!(capture.node.kind(), "preproc_def" | "preproc_function_def")
                {
                    let Some(end) = c::macro_end(
                        capture.node,
                        input.source,
                        c_included_ranges.as_deref().unwrap_or(&[]),
                        dialect,
                        input.cancellation,
                    )?
                    else {
                        continue;
                    };
                    Some(end)
                } else {
                    None
                };
                if let Some(dialect) = c_dialect
                    && matches!(capture.node.kind(), "preproc_def" | "preproc_function_def")
                    && matches!(
                        role,
                        StructuralRole::Signature | StructuralRole::StringLiteral
                    )
                {
                    let mode = if role == StructuralRole::Signature {
                        c::ReplacementMode::Gaps
                    } else {
                        c::ReplacementMode::Lexical
                    };
                    let generated = c::replacement_candidates(
                        capture.node,
                        input.source,
                        c_macro_end.ok_or_else(|| query_failure("query-c-preprocessor-owner"))?,
                        mode,
                        limits.captures.saturating_sub(capture_count),
                        dialect,
                        input.cancellation,
                        &mut |candidate| {
                            capture_count = capture_count
                                .checked_add(1)
                                .ok_or_else(|| query_failure("query-capture-accounting"))?;
                            retain(candidate)
                        },
                    )?;
                    if generated.exhausted {
                        refusal = Some(IdentityRefusal::generated_slots(
                            capture_count,
                            limits.captures,
                        ));
                        limit = Some(QueryLimit::Capture);
                        break 'query;
                    }
                    if role == StructuralRole::StringLiteral {
                        continue;
                    }
                }
                if input.family == GrammarFamily::Groovy
                    && !groovy_calls.retain(capture.node, role, input.cancellation)?
                {
                    continue;
                }
                if input.family == GrammarFamily::Thrift
                    && !thrift::retain_capture(
                        capture.node,
                        role,
                        input.source,
                        input.cancellation,
                    )?
                {
                    continue;
                }
                if input.family == GrammarFamily::Graphql
                    && !graphql::retain_capture(
                        capture.node,
                        role,
                        input.source,
                        input.cancellation,
                    )?
                {
                    continue;
                }
                if input.family == GrammarFamily::Elixir
                    && !elixir::retain_capture(
                        capture.node,
                        role,
                        input.source,
                        input.cancellation,
                    )?
                {
                    continue;
                }
                if input.family == GrammarFamily::Protobuf
                    && !protobuf::retain_capture(
                        capture.node,
                        role,
                        input.source,
                        input.cancellation,
                    )?
                {
                    continue;
                }
                let candidate = if input.family == GrammarFamily::Protobuf {
                    protobuf::candidate(capture.node, role, input.source, input.cancellation)
                } else if input.family == GrammarFamily::Thrift {
                    thrift::candidate(capture.node, role, input.source, input.cancellation)
                } else if input.family == GrammarFamily::Elixir {
                    elixir::candidate(capture.node, role, input.source, input.cancellation)
                } else if input.family == GrammarFamily::Wgsl {
                    wgsl::candidate(
                        capture.node,
                        role,
                        input.source,
                        input.cancellation,
                        &mut wgsl_captures,
                    )
                } else if input.family == GrammarFamily::Hlsl {
                    hlsl::candidate(
                        capture.node,
                        role,
                        input.source,
                        input.cancellation,
                        &mut hlsl_captures,
                    )
                } else if input.family == GrammarFamily::Glsl {
                    glsl::candidate(
                        capture.node,
                        role,
                        input.source,
                        input.cancellation,
                        &mut glsl_captures,
                    )
                } else if input.family == GrammarFamily::Assembly {
                    assembly::candidate(
                        capture.node,
                        role,
                        input.source,
                        input.cancellation,
                        &mut assembly_captures,
                    )
                } else if input.family == GrammarFamily::CommonLisp {
                    common_lisp::candidate(
                        capture.node,
                        role,
                        input.source,
                        input.cancellation,
                        &mut common_lisp_captures,
                    )
                } else if input.family == GrammarFamily::Scheme {
                    scheme::candidate(
                        capture.node,
                        role,
                        input.source,
                        input.cancellation,
                        &mut scheme_captures,
                    )
                } else if input.family == GrammarFamily::Pascal {
                    pascal::candidate(
                        capture.node,
                        role,
                        input.source,
                        input.cancellation,
                        &mut pascal_captures,
                    )
                } else if input.family == GrammarFamily::Apex {
                    apex::candidate(
                        capture.node,
                        role,
                        input.source,
                        input.cancellation,
                        &mut apex_captures,
                    )
                } else if input.family == GrammarFamily::Sas {
                    sas::candidate(
                        capture.node,
                        role,
                        input.source,
                        input.cancellation,
                        &mut sas_captures,
                    )
                } else if matches!(
                    input.family,
                    GrammarFamily::JavaScript | GrammarFamily::TypeScript
                ) && let Some(candidate) = jsx::candidate(
                    input.family,
                    capture.node,
                    role,
                    input.source,
                    jsx_complete_source,
                    input.cancellation,
                )? {
                    Ok(candidate)
                } else if input.family == GrammarFamily::Rust
                    && let Some(syntax) = rust_captures.syntax(
                        capture.node,
                        role,
                        input.source,
                        input.cancellation,
                    )?
                {
                    Ok(QueryCandidate {
                        start: capture.node.start_byte(),
                        end: capture.node.end_byte(),
                        role,
                        syntax,
                        required: false,
                        native_depth: 0,
                    })
                } else {
                    candidate_for_capture(
                        input.family,
                        capture,
                        role,
                        input.source,
                        input.cancellation,
                        &mut fsharp_captures,
                        &mut plsql_captures,
                    )
                };
                let candidate = candidate?;
                if fsharp_captures.exhausted()
                    || sas_captures.exhausted()
                    || apex_captures.exhausted()
                    || pascal_captures.exhausted()
                    || common_lisp_captures.exhausted()
                    || scheme_captures.exhausted()
                    || assembly_captures.exhausted()
                    || glsl_captures.exhausted()
                    || hlsl_captures.exhausted()
                    || wgsl_captures.exhausted()
                {
                    let origin = [
                        (fsharp_captures.exhausted(), RefusalOrigin::FsharpCaptures),
                        (sas_captures.exhausted(), RefusalOrigin::SasCaptures),
                        (apex_captures.exhausted(), RefusalOrigin::ApexCaptures),
                        (pascal_captures.exhausted(), RefusalOrigin::PascalCaptures),
                        (
                            common_lisp_captures.exhausted(),
                            RefusalOrigin::CommonLispCaptures,
                        ),
                        (scheme_captures.exhausted(), RefusalOrigin::SchemeCaptures),
                        (
                            assembly_captures.exhausted(),
                            RefusalOrigin::AssemblyCaptures,
                        ),
                        (glsl_captures.exhausted(), RefusalOrigin::GlslCaptures),
                        (hlsl_captures.exhausted(), RefusalOrigin::HlslCaptures),
                        (wgsl_captures.exhausted(), RefusalOrigin::WgslCaptures),
                    ]
                    .into_iter()
                    .find_map(|(exhausted, origin)| exhausted.then_some(origin))
                    .expect("the guarded helper refusal has an origin");
                    refusal = Some(IdentityRefusal::helper(origin));
                    limit = Some(QueryLimit::Capture);
                    break 'query;
                }
                let mut candidate = candidate;
                canonicalize_file_root_preamble(
                    &mut candidate,
                    capture.node,
                    input.source,
                    complete_file_input,
                );
                if input.family == GrammarFamily::PowerShell {
                    let keep = powershell_formals.refine(
                        capture.node,
                        role,
                        &mut candidate,
                        input.cancellation,
                    )?;
                    if powershell_formals.exhausted() {
                        refusal = Some(IdentityRefusal::checked_helper(
                            RefusalOrigin::PowerShellFormals,
                            powershell_formals.initial_work(),
                            powershell_formals.remaining_work(),
                        ));
                        limit = Some(QueryLimit::Capture);
                        break 'query;
                    }
                    if !keep {
                        continue;
                    }
                }
                if input.family == GrammarFamily::C {
                    let decision = c_variables.refine(
                        capture.node,
                        role,
                        &mut candidate,
                        input.cancellation,
                    )?;
                    if c_variables.exhausted() {
                        refusal = Some(IdentityRefusal::checked_helper(
                            RefusalOrigin::CVariables,
                            limits.captures.min(limits.body_nodes),
                            c_variables.remaining_work(),
                        ));
                        limit = Some(QueryLimit::Capture);
                        break 'query;
                    }
                    if let Some(keep) = decision {
                        if keep {
                            retain(candidate)?;
                        }
                        continue;
                    }
                    let keep = c_formal_reads.refine(
                        capture.node,
                        role,
                        &mut candidate,
                        input.cancellation,
                    )?;
                    if c_formal_reads.exhausted() {
                        refusal = Some(IdentityRefusal::checked_helper(
                            RefusalOrigin::CFormalReads,
                            limits.captures.min(limits.body_nodes),
                            c_formal_reads.remaining_work(),
                        ));
                        limit = Some(QueryLimit::Capture);
                        break 'query;
                    }
                    if !keep {
                        continue;
                    }
                }
                if input.family == GrammarFamily::Cpp {
                    let keep = cpp_const_formals.refine(
                        capture.node,
                        role,
                        &mut candidate,
                        input.cancellation,
                    )?;
                    if cpp_const_formals.exhausted() {
                        refusal = Some(IdentityRefusal::helper(RefusalOrigin::CppConstFormals));
                        limit = Some(QueryLimit::Capture);
                        break 'query;
                    }
                    if !keep {
                        continue;
                    }
                }
                if input.family == GrammarFamily::CSharp {
                    let keep = csharp_formals.refine(
                        capture.node,
                        role,
                        &mut candidate,
                        input.cancellation,
                    )?;
                    if csharp_formals.exhausted() {
                        refusal = Some(IdentityRefusal::helper(RefusalOrigin::CsharpFormals));
                        limit = Some(QueryLimit::Capture);
                        break 'query;
                    }
                    if !keep {
                        continue;
                    }
                }
                if input.family == GrammarFamily::Scala {
                    let keep = scala_formals.refine(
                        capture.node,
                        role,
                        &mut candidate,
                        input.cancellation,
                    )?;
                    if scala_formals.exhausted() {
                        refusal = Some(IdentityRefusal::helper(RefusalOrigin::ScalaFormals));
                        limit = Some(QueryLimit::Capture);
                        break 'query;
                    }
                    if !keep {
                        continue;
                    }
                }
                if input.family == GrammarFamily::Python {
                    let keep = python_variables.refine(
                        input.tree.root_node(),
                        capture.node,
                        role,
                        &mut candidate,
                        input.cancellation,
                    )?;
                    if python_variables.exhausted() {
                        refusal = Some(IdentityRefusal::helper(
                            RefusalOrigin::PythonModuleVariables,
                        ));
                        limit = Some(QueryLimit::Capture);
                        break 'query;
                    }
                    if !keep {
                        continue;
                    }
                    python_parameter_reads.refine(
                        capture.node,
                        role,
                        &mut candidate,
                        input.cancellation,
                    )?;
                    if python_parameter_reads.exhausted() {
                        refusal =
                            Some(IdentityRefusal::helper(RefusalOrigin::PythonParameterReads));
                        limit = Some(QueryLimit::Capture);
                        break 'query;
                    }
                }
                if input.family == GrammarFamily::Php {
                    let keep = php_this_calls.refine(
                        capture.node,
                        role,
                        &mut candidate,
                        input.cancellation,
                    )?;
                    if php_this_calls.exhausted() {
                        refusal = Some(IdentityRefusal::helper(RefusalOrigin::PhpThisCalls));
                        limit = Some(QueryLimit::Capture);
                        break 'query;
                    }
                    if keep == Some(false) {
                        continue;
                    }
                }
                if input.family == GrammarFamily::Go {
                    let keep = match go_calls.refine(
                        capture.node,
                        role,
                        &mut candidate,
                        input.cancellation,
                    )? {
                        Some(keep) => keep,
                        None => go_formal_reads.refine(
                            capture.node,
                            role,
                            &mut candidate,
                            input.cancellation,
                        )?,
                    };
                    if go_calls.exhausted() {
                        refusal = Some(IdentityRefusal::helper(RefusalOrigin::GoCalls));
                        limit = Some(QueryLimit::Capture);
                        break 'query;
                    }
                    if go_formal_reads.exhausted() {
                        refusal = Some(IdentityRefusal::helper(RefusalOrigin::GoFormalReads));
                        limit = Some(QueryLimit::Capture);
                        break 'query;
                    }
                    if !keep {
                        continue;
                    }
                }
                if input.family == GrammarFamily::Java {
                    java_parameter_reads.refine(
                        capture.node,
                        role,
                        &mut candidate,
                        input.cancellation,
                    )?;
                    if java_parameter_reads.exhausted() {
                        refusal = Some(IdentityRefusal::helper(RefusalOrigin::JavaParameterReads));
                        limit = Some(QueryLimit::Capture);
                        break 'query;
                    }
                }
                if input.family == GrammarFamily::Java {
                    let keep = java_local_receivers.refine(
                        capture.node,
                        role,
                        &mut candidate,
                        input.cancellation,
                    )?;
                    if java_local_receivers.exhausted() {
                        refusal = Some(IdentityRefusal::helper(RefusalOrigin::JavaLocalReceivers));
                        limit = Some(QueryLimit::Capture);
                        break 'query;
                    }
                    if !keep {
                        continue;
                    }
                }
                if input.family == GrammarFamily::Bash && role == StructuralRole::Reference {
                    if let Some(syntax) = bash_reads.syntax(capture.node, input.cancellation)? {
                        candidate.syntax = syntax;
                    }
                    if bash_reads.exhausted() {
                        refusal = Some(IdentityRefusal::helper(RefusalOrigin::BashReads));
                        limit = Some(QueryLimit::Capture);
                        break 'query;
                    }
                }
                if input.family == GrammarFamily::TypeScript && role == StructuralRole::Call {
                    if typescript_calls.direct_call(capture.node, input.cancellation)? {
                        candidate.syntax = "typescript.direct_module_call";
                    }
                    if typescript_calls.exhausted() {
                        refusal = Some(IdentityRefusal::helper(RefusalOrigin::TypescriptCalls));
                        limit = Some(QueryLimit::Capture);
                        break 'query;
                    }
                }
                if input.family == GrammarFamily::C && role == StructuralRole::Call {
                    if c_calls.direct_call(capture.node, input.cancellation)? {
                        candidate.syntax = "c.direct_file_call";
                    }
                    if c_calls.exhausted() {
                        refusal = Some(IdentityRefusal::helper(RefusalOrigin::CCalls));
                        limit = Some(QueryLimit::Capture);
                        break 'query;
                    }
                }
                if let Some(dialect) = c_dialect
                    && let Some(syntax) = c::macro_syntax(capture.node, role, dialect)
                {
                    candidate.syntax = syntax;
                    candidate.end =
                        c_macro_end.ok_or_else(|| query_failure("query-c-preprocessor-owner"))?;
                }
                if input.family == GrammarFamily::Rust {
                    rust_captures.adjust(capture.node, &mut candidate, input.cancellation)?;
                }
                if input.family == GrammarFamily::Groovy && candidate.syntax == "groovy.field" {
                    candidate.syntax = groovy_members.kind(capture.node, input.cancellation)?;
                }
                if input.family == GrammarFamily::Groovy {
                    if !groovy_callables.adjust(
                        capture.node,
                        &mut candidate,
                        input.source,
                        input.cancellation,
                    )? {
                        continue;
                    }
                    groovy_calls.adjust(capture.node, &mut candidate, input.cancellation)?;
                    groovy::adjust_candidate(
                        capture.node,
                        &mut candidate,
                        input.source,
                        input.cancellation,
                    )?;
                }
                if input.family == GrammarFamily::Groovy
                    && capture.node.kind() == "package_declaration"
                    && matches!(role, StructuralRole::Module | StructuralRole::Scope)
                {
                    if groovy_package == Some(capture.node) {
                        // The package owns the remaining compilation unit, not just
                        // its header. Definition captures keep their written bytes.
                        candidate.end = input.tree.root_node().end_byte();
                        // At byte zero, file and package extents coincide. The
                        // validated package is a direct native child of the file.
                        candidate.native_depth = 1;
                    } else if role == StructuralRole::Scope {
                        candidate.syntax = "groovy.package_ownership_unavailable";
                    }
                }
                if input.family == GrammarFamily::Markdown
                    && role == StructuralRole::Declaration
                    && matches!(capture.node.kind(), "atx_heading" | "setext_heading")
                {
                    candidate.syntax = "markdown.section";
                    candidate.end = markdown::section_end(capture.node, input.cancellation)?;
                }
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
                if input.family == GrammarFamily::Groovy
                    && role == StructuralRole::Reference
                    && capture.node.kind() == "identifier"
                    && candidate.syntax == "groovy.identifier"
                    && let Some(syntax) = groovy::value_context(capture.node, input.cancellation)?
                {
                    retain(QueryCandidate {
                        role: StructuralRole::Expression,
                        syntax,
                        ..candidate
                    })?;
                }
                if input.family == GrammarFamily::Groovy
                    && role == StructuralRole::Expression
                    && matches!(
                        capture.node.kind(),
                        "method_declaration" | "constructor_declaration"
                    )
                    && candidate.syntax.starts_with("groovy.callable_")
                {
                    retain(QueryCandidate {
                        syntax: groovy::callable_return(
                            capture.node,
                            input.source,
                            input.cancellation,
                        )?,
                        ..candidate
                    })?;
                }
                retain(candidate)?;
            }
        }
        drop(matches);
        if callback_cancelled {
            input.cancellation.check()?;
        }
        input.cancellation.check()?;
        let cursor_exceeded = cursor.did_exceed_match_limit();
        if cursor_exceeded && refusal.is_none() {
            refusal = Some(if limit == Some(QueryLimit::Match) {
                IdentityRefusal::yielded_matches(match_count, limits.matches)
            } else {
                IdentityRefusal::cursor()
            });
        }
        if let Some(mut event) = refusal {
            // The legacy counters also charge generated candidates. Keep raw
            // native progress separate and never invent the cursor's peak.
            event.progress(
                native_matches,
                native_captures,
                capture_count.checked_sub(native_captures),
                cursor_exceeded,
            );
            if let Some(witness) = witness {
                *witness = Some(event);
            }
        }
        if cursor_exceeded {
            Ok(Some(QueryLimit::CursorMatch))
        } else {
            Ok(limit)
        }
    }
}

fn identity_scan_limits(max_nodes: usize) -> QueryScanLimits {
    QueryScanLimits {
        matches: max_nodes.saturating_mul(8).min(HARD_MAX_QUERY_MATCHES),
        captures: max_nodes.saturating_mul(8).min(HARD_MAX_QUERY_CAPTURES),
        body_nodes: max_nodes,
    }
}

pub(crate) struct QueryPackRegistry {
    packs: Vec<(GrammarFamily, QueryPackEntry)>,
    typescript_tsx: QueryPackEntry,
}

struct QueryPackEntry {
    family: GrammarFamily,
    native: GrammarFamily,
    source: &'static str,
    compiled: OnceLock<Result<QueryPack, GrammarFamily>>,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum QueryPackError {
    #[error("query pack is missing for {family:?} with native grammar {native:?}")]
    Missing {
        family: GrammarFamily,
        native: GrammarFamily,
    },
    #[error("query compilation failed for {family:?} with native grammar {native:?}")]
    Compilation {
        family: GrammarFamily,
        native: GrammarFamily,
    },
    #[error(transparent)]
    Cancelled(#[from] Cancelled),
}

impl From<QueryPackError> for AdapterError {
    fn from(error: QueryPackError) -> Self {
        error.into_adapter_error("query-pack-missing")
    }
}

impl QueryPackError {
    pub(crate) fn into_adapter_error(self, missing_code: &'static str) -> AdapterError {
        match self {
            Self::Missing { .. } => query_failure(missing_code),
            Self::Compilation { .. } => query_failure("query-pack-compile"),
            Self::Cancelled(cancelled) => cancelled.into(),
        }
    }
}

impl QueryPackEntry {
    const fn new(family: GrammarFamily, native: GrammarFamily, source: &'static str) -> Self {
        Self {
            family,
            native,
            source,
            compiled: OnceLock::new(),
        }
    }

    fn load(&self, cancellation: &Cancellation) -> Result<&QueryPack, QueryPackError> {
        self.load_with(cancellation, || {
            if self.family == self.native {
                QueryPack::compile(self.family, self.source)
            } else {
                QueryPack::compile_native(self.family, self.native, self.source)
            }
        })
    }

    fn load_with(
        &self,
        cancellation: &Cancellation,
        initialize: impl FnOnce() -> Result<QueryPack, GrammarFamily>,
    ) -> Result<&QueryPack, QueryPackError> {
        cancellation.check()?;
        // Query compilation is native, non-preemptible work. A caller cancelled
        // while compiling or waiting must not consume the result; only the
        // immutable built-in query is retained for other callers.
        let compiled = self.compiled.get_or_init(initialize);
        cancellation.check()?;
        compiled.as_ref().map_err(|_| QueryPackError::Compilation {
            family: self.family,
            native: self.native,
        })
    }
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

fn canonicalize_file_root_preamble(
    candidate: &mut QueryCandidate,
    node: tree_sitter::Node<'_>,
    source: &[u8],
    complete_file_input: bool,
) {
    if complete_file_input
        && node.parent().is_none()
        && !node.is_error()
        && !node.is_missing()
        && !node.has_error()
        && (matches!(
            candidate.role,
            StructuralRole::Root | StructuralRole::Module | StructuralRole::Scope
        ) || candidate.role == StructuralRole::Signature
            && candidate.syntax == "rust.proof_context")
        && source.starts_with(b"\xef\xbb\xbf")
        && node.start_byte() == 3
        && node.end_byte() == source.len()
        && candidate.start == node.start_byte()
        && candidate.end == node.end_byte()
    {
        // A known BOM belongs to the complete file envelope, not its declarations.
        candidate.start = 0;
    }
}

fn candidate_for_capture(
    family: GrammarFamily,
    capture: QueryCapture<'_>,
    role: StructuralRole,
    source: &[u8],
    cancellation: &Cancellation,
    fsharp_captures: &mut fsharp::CaptureCache,
    plsql_captures: &mut plsql::CaptureCache,
) -> Result<QueryCandidate, AdapterError> {
    if family == GrammarFamily::Sql
        && role == StructuralRole::Signature
        && sql::recovered_body(capture.node, cancellation)?
    {
        return Ok(QueryCandidate {
            start: capture.node.start_byte(),
            end: capture.node.end_byte(),
            role,
            syntax: "sql.recovered_body",
            required: true,
            native_depth: 0,
        });
    }
    if family == GrammarFamily::ObjectiveC
        && let Some(candidate) = objective_c::unavailable_declarator(capture.node, role)
    {
        return Ok(candidate);
    }
    if family == GrammarFamily::Graphql {
        return graphql::candidate(capture.node, role, source, cancellation);
    }
    if family == GrammarFamily::Capnp {
        return capnp::candidate(capture.node, role, source, cancellation);
    }
    if family == GrammarFamily::PlSql {
        return plsql::candidate(capture.node, role, source, cancellation, plsql_captures);
    }
    if family == GrammarFamily::Fortran {
        return fortran::candidate(capture.node, role, source, cancellation);
    }
    if family.is_classic_basic()
        && role == StructuralRole::Reference
        && let Some(range) = vba::qualified_pointer_range(capture.node, source, cancellation)?
    {
        return Ok(QueryCandidate {
            start: range.start,
            end: range.end,
            role,
            syntax: "vba.qualified_procedure_pointer",
            required: false,
            native_depth: 0,
        });
    }
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
        StructuralRole::Signature
            if matches!(
                family,
                GrammarFamily::JavaScript | GrammarFamily::TypeScript
            ) =>
        {
            ecmascript::binding_signature_syntax(family, capture.node)
                .or(ecmascript::import_signature_syntax(family, capture.node))
                .or(ecmascript::export_signature_syntax(
                    family,
                    capture.node,
                    cancellation,
                )?)
                .or_else(|| canonical_syntax(family, capture.node.kind()))
                .ok_or_else(|| query_failure("query-node-kind"))?
        }
        StructuralRole::Reference
            if matches!(
                family,
                GrammarFamily::JavaScript | GrammarFamily::TypeScript
            ) =>
        {
            if let Some(syntax) =
                ecmascript::qualified_reference_syntax(family, capture.node, cancellation)?
            {
                syntax
            } else if family == GrammarFamily::TypeScript
                && capture
                    .node
                    .parent()
                    .is_some_and(|parent| parent.kind() == "type_query")
            {
                "typescript.type_query_value"
            } else if let Some(syntax) =
                ecmascript::export_reference_syntax(family, capture.node, cancellation)?
            {
                syntax
            } else if ecmascript::is_foreign_import_name(capture.node) {
                if family == GrammarFamily::TypeScript {
                    "typescript.import_name"
                } else {
                    "javascript.import_name"
                }
            } else {
                canonical_syntax(family, capture.node.kind())
                    .ok_or_else(|| query_failure("query-node-kind"))?
            }
        }
        StructuralRole::Scope
            if family == GrammarFamily::PowerShell
                && capture.node.kind() == "key_expression"
                && !powershell::is_literal_key(capture.node) =>
        {
            "powershell.dynamic_key"
        }
        StructuralRole::Declaration
            if family == GrammarFamily::Python && capture.node.kind() == "identifier" =>
        {
            "python.parameter"
        }
        StructuralRole::Declaration
            if family == GrammarFamily::Php && capture.node.kind() == "method_declaration" =>
        {
            if is_php_named_class_constructor(capture.node, source) {
                "php.constructor"
            } else {
                "php.method"
            }
        }
        StructuralRole::Declaration if family == GrammarFamily::PowerShell => {
            powershell::declaration_syntax(capture.node, source)
                .ok_or_else(|| query_failure("query-powershell-declaration-kind"))?
        }
        StructuralRole::Declaration
            if matches!(
                family,
                GrammarFamily::JavaScript | GrammarFamily::TypeScript
            ) && matches!(
                capture.node.kind(),
                "identifier" | "shorthand_property_identifier_pattern"
            ) =>
        {
            let binding = ecmascript::binding_kind(capture.node, cancellation)?
                .ok_or_else(|| query_failure("query-ecmascript-binding-kind"))?;
            match (family == GrammarFamily::TypeScript, binding) {
                (true, ecmascript::BindingKind::Parameter) => "typescript.parameter",
                (false, ecmascript::BindingKind::Parameter) => "javascript.parameter",
                (true, ecmascript::BindingKind::Variable) => "typescript.variable",
                (false, ecmascript::BindingKind::Variable) => "javascript.variable",
                (true, ecmascript::BindingKind::Import { type_only: false }) => {
                    "typescript.import_binding"
                }
                (false, ecmascript::BindingKind::Import { type_only: false }) => {
                    "javascript.import_binding"
                }
                (true, ecmascript::BindingKind::Import { type_only: true }) => {
                    "typescript.type_import_binding"
                }
                (false, ecmascript::BindingKind::Import { type_only: true }) => {
                    "javascript.type_import_binding"
                }
            }
        }
        StructuralRole::Declaration
            if matches!(
                family,
                GrammarFamily::JavaScript | GrammarFamily::TypeScript
            ) && matches!(
                capture.node.kind(),
                "function_expression" | "generator_function"
            ) =>
        {
            if family == GrammarFamily::TypeScript {
                "typescript.function"
            } else {
                "javascript.function"
            }
        }
        _ if family == GrammarFamily::Groovy => {
            groovy::syntax(capture.node, role).ok_or_else(|| query_failure("query-groovy-kind"))?
        }
        _ if family.is_classic_basic() => {
            if role == StructuralRole::Expression {
                if capture.node.kind() == "assignment_statement" {
                    "vba.implicit_comparison_statement"
                } else {
                    vba::declared_visibility(capture.node, source, cancellation)?
                }
            } else if role == StructuralRole::Module
                && capture.node.kind() == "source_file"
                && vba::class_header(capture.node, source, cancellation)?.is_some()
            {
                "vba.class"
            } else if role == StructuralRole::Reference && capture.node.kind() == "type_expression"
            {
                vba::implemented_type_syntax(capture.node, source, cancellation)?
            } else if role == StructuralRole::Reference
                && matches!(
                    capture.node.kind(),
                    "identifier"
                        | "bang_identifier"
                        | "line_number_literal"
                        | "dotted_type_expression"
                )
            {
                vba::reference_syntax(capture.node, source, cancellation)?
            } else if role == StructuralRole::Reference
                && matches!(
                    capture.node.kind(),
                    "qualified_member_expression" | "implicit_member_expression"
                )
            {
                vba::member_syntax(capture.node, source, cancellation)?
            } else {
                vba::syntax(capture.node, role).ok_or_else(|| query_failure("query-vba-kind"))?
            }
        }
        _ if family == GrammarFamily::Perl => {
            perl::syntax(capture.node, role, source, cancellation)?
                .ok_or_else(|| query_failure("query-perl-kind"))?
        }
        _ if family == GrammarFamily::Matlab => {
            matlab::syntax(capture.node, role, source, cancellation)?
                .ok_or_else(|| query_failure("query-matlab-kind"))?
        }
        _ if family == GrammarFamily::Nix => nix::syntax(capture.node, role, source, cancellation)?
            .ok_or_else(|| query_failure("query-nix-kind"))?,
        _ if family == GrammarFamily::Haskell => {
            haskell::syntax(capture.node, role, source, cancellation)?
                .ok_or_else(|| query_failure("query-haskell-kind"))?
        }
        _ if matches!(
            family,
            GrammarFamily::FSharp | GrammarFamily::FSharpSignature
        ) =>
        {
            fsharp::syntax(capture.node, role, source, cancellation, fsharp_captures)?
                .ok_or_else(|| query_failure("query-fsharp-kind"))?
        }
        _ if matches!(family, GrammarFamily::CobolFixed | GrammarFamily::CobolFree) => {
            cobol::syntax(capture.node, role).ok_or_else(|| query_failure("query-cobol-kind"))?
        }
        _ if family == GrammarFamily::ObjectiveC => objective_c::capture_syntax(capture.node, role)
            .ok_or_else(|| query_failure("query-objective-c-kind"))?,
        StructuralRole::Declaration if family == GrammarFamily::Dart => {
            dart::declaration_syntax(capture.node)
                .ok_or_else(|| query_failure("query-dart-declaration-kind"))?
        }
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
        StructuralRole::Declaration
            if family == GrammarFamily::CSharp
                && capture.node.kind() == "variable_declarator"
                && capture
                    .node
                    .parent()
                    .filter(|node| node.kind() == "variable_declaration")
                    .and_then(|node| node.parent())
                    .is_some_and(|node| node.kind() == "field_declaration") =>
        {
            "csharp.field"
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
        StructuralRole::Scope if family.is_json() => {
            match capture.node.parent().map(|node| node.kind()) {
                Some("array") if family == GrammarFamily::Jsonc => "jsonc.array_element",
                Some("document") if family == GrammarFamily::Jsonc => "jsonc.document_value",
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
            GrammarFamily::Dart => "dart.call_name",
            GrammarFamily::PowerShell => "powershell.call_name",
            GrammarFamily::Fortran => "fortran.subroutine_name",
            _ => return Err(query_failure("query-call-name-family")),
        },
        StructuralRole::Call => match family {
            GrammarFamily::Clojure => return Err(query_failure("query-clojure-unproven-call")),
            GrammarFamily::Racket => return Err(query_failure("query-racket-unproven-call")),
            GrammarFamily::Graphql => return Err(query_failure("query-graphql-unproven-call")),
            GrammarFamily::Elixir => return Err(query_failure("query-elixir-unproven-call")),
            GrammarFamily::Protobuf => return Err(query_failure("query-protobuf-unproven-call")),
            GrammarFamily::Thrift => return Err(query_failure("query-thrift-unproven-call")),
            GrammarFamily::Capnp => return Err(query_failure("query-capnp-unproven-call")),
            GrammarFamily::Rust => "rust.call",
            GrammarFamily::Python => "python.call",
            GrammarFamily::JavaScript => "javascript.call",
            GrammarFamily::Java => "java.call",
            GrammarFamily::Apex => return Err(query_failure("query-apex-unproven-call")),
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
            GrammarFamily::Dart => "dart.call",
            GrammarFamily::PowerShell => "powershell.call",
            GrammarFamily::Fortran => "fortran.explicit_subroutine",
            GrammarFamily::PlSql => return Err(query_failure("query-plsql-unproven-call")),
            GrammarFamily::Sas => return Err(query_failure("query-sas-unproven-call")),
            GrammarFamily::Assembly => return Err(query_failure("query-assembly-unproven-call")),
            GrammarFamily::Glsl => return Err(query_failure("query-glsl-unproven-call")),
            GrammarFamily::Hlsl => return Err(query_failure("query-hlsl-unproven-call")),
            GrammarFamily::Wgsl => return Err(query_failure("query-wgsl-unproven-call")),
            GrammarFamily::Xml | GrammarFamily::Dtd => {
                return Err(query_failure("query-xml-unproven-call"));
            }
            GrammarFamily::CMake => return Err(query_failure("query-cmake-unproven-call")),
            GrammarFamily::Coq => return Err(query_failure("query-coq-unproven-call")),
            GrammarFamily::Julia => return Err(query_failure("query-julia-unproven-call")),
            GrammarFamily::Eiffel => return Err(query_failure("query-eiffel-unproven-call")),
            GrammarFamily::V => return Err(query_failure("query-v-unproven-call")),
            GrammarFamily::Vhdl => return Err(query_failure("query-vhdl-unproven-call")),
            GrammarFamily::Hcl => return Err(query_failure("query-hcl-unproven-call")),
            GrammarFamily::JsonSchema => {
                return Err(query_failure("query-json-schema-unproven-call"));
            }
            GrammarFamily::Chapel => {
                return Err(query_failure("query-chapel-overload-proof-required"));
            }
            GrammarFamily::Dockerfile => {
                return Err(query_failure("query-dockerfile-call-unproven"));
            }
            GrammarFamily::Idris => {
                return Err(query_failure("query-idris-application-call-unqualified"));
            }
            GrammarFamily::Sed => return Err(query_failure("query-sed-call-unimplemented")),
            GrammarFamily::Wolfram => return Err(query_failure("query-wolfram-call-unproven")),
            GrammarFamily::Pug => return Err(query_failure("query-pug-call-unproven")),
            GrammarFamily::Lean => return Err(query_failure("query-lean-call-unproven")),
            GrammarFamily::Coffeescript => {
                return Err(query_failure("query-coffeescript-call-unproven"));
            }
            GrammarFamily::Meson => return Err(query_failure("query-meson-call-unproven")),
            GrammarFamily::Gdscript => return Err(query_failure("query-gdscript-call-unproven")),
            GrammarFamily::Qsharp => {
                return Err(query_failure("query-qsharp-native-call-required"));
            }
            GrammarFamily::StandardMl | GrammarFamily::Wat => {
                return Err(query_failure("query-sml-wat-native-call-required"));
            }
            GrammarFamily::Mojo => return Err(query_failure("query-mojo-native-call-required")),
            GrammarFamily::Haxe => return Err(query_failure("query-haxe-native-call-required")),
            GrammarFamily::Qml => return Err(query_failure("query-qml-native-call-required")),
            GrammarFamily::Ring => return Err(query_failure("query-ring-unproven-call")),
            GrammarFamily::Hare => return Err(query_failure("query-hare-native-call-required")),
            GrammarFamily::Modelica => return Err(query_failure("query-modelica-unproven-call")),
            GrammarFamily::D => return Err(query_failure("query-d-unproven-call")),
            GrammarFamily::Cue => return Err(query_failure("query-cue-unproven-call")),
            GrammarFamily::Ada => return Err(query_failure("query-ada-unproven-call")),
            GrammarFamily::Zig => return Err(query_failure("query-zig-unproven-call")),
            GrammarFamily::Crystal => return Err(query_failure("query-crystal-unproven-call")),
            GrammarFamily::Pony => return Err(query_failure("query-pony-unproven-call")),
            GrammarFamily::Fish => return Err(query_failure("query-fish-unproven-call")),
            GrammarFamily::Nim => return Err(query_failure("query-nim-unproven-call")),
            GrammarFamily::Verilog | GrammarFamily::SystemVerilog => {
                return Err(query_failure("query-hdl-unproven-call"));
            }
            GrammarFamily::Pascal => return Err(query_failure("query-pascal-unproven-call")),
            GrammarFamily::CommonLisp => {
                return Err(query_failure("query-common-lisp-unproven-call"));
            }
            GrammarFamily::Scheme => return Err(query_failure("query-scheme-unproven-call")),
            GrammarFamily::Ocaml | GrammarFamily::OcamlInterface => {
                return Err(query_failure("query-ocaml-unproven-call"));
            }
            GrammarFamily::Css => return Err(query_failure("query-css-call-kind")),
            GrammarFamily::Json | GrammarFamily::Jsonc => {
                return Err(query_failure("query-json-call-kind"));
            }
            GrammarFamily::Toml => return Err(query_failure("query-toml-call-kind")),
            GrammarFamily::Yaml => return Err(query_failure("query-yaml-call-kind")),
            GrammarFamily::Html => return Err(query_failure("query-html-call-kind")),
            GrammarFamily::Sql => return Err(query_failure("query-sql-call-kind")),
            GrammarFamily::Markdown => return Err(query_failure("query-markdown-call-kind")),
            GrammarFamily::Astro => return Err(query_failure("query-astro-call-kind")),
            GrammarFamily::ObjectiveC => "objective_c.call",
            GrammarFamily::Nix => "nix.call",
            GrammarFamily::Matlab => return Err(query_failure("query-matlab-ambiguous-call")),
            GrammarFamily::Perl => return Err(query_failure("query-perl-unresolved-call")),
            GrammarFamily::Groovy => "groovy.call",
            GrammarFamily::Haskell => "haskell.application",
            GrammarFamily::FSharp | GrammarFamily::FSharpSignature => {
                return Err(query_failure("query-fsharp-unproven-call"));
            }
            GrammarFamily::CobolFixed | GrammarFamily::CobolFree => {
                return Err(query_failure("query-cobol-unproven-call"));
            }
            GrammarFamily::Vba | GrammarFamily::VisualBasicClassic => {
                return Err(query_failure("query-vba-ambiguous-call"));
            }
        },
        _ => canonical_syntax(family, capture.node.kind())
            .ok_or_else(|| query_failure("query-node-kind"))?,
    };
    let mut start = capture.node.start_byte();
    let mut end = capture.node.end_byte();
    if matches!(
        family,
        GrammarFamily::FSharp | GrammarFamily::FSharpSignature
    ) {
        let range =
            fsharp::capture_range(capture.node, role, source, cancellation, fsharp_captures)?
                .ok_or_else(|| query_failure("query-fsharp-range"))?;
        start = range.start;
        end = range.end;
    }
    if family == GrammarFamily::Haskell {
        let range = haskell::capture_range(capture.node, role, cancellation)?
            .ok_or_else(|| query_failure("query-haskell-range"))?;
        start = range.start;
        end = range.end;
    }
    if matches!(family, GrammarFamily::CobolFixed | GrammarFamily::CobolFree) {
        let range = cobol::capture_range(capture.node, role, source, cancellation)?
            .ok_or_else(|| query_failure("query-cobol-range"))?;
        start = range.start;
        end = range.end;
    }
    if family == GrammarFamily::Matlab && end == source.len().saturating_add(1) {
        // MATLAB's parser-only EOF delimiter cannot become source evidence.
        end = source.len();
    }
    if family.is_classic_basic()
        && matches!(role, StructuralRole::Signature | StructuralRole::Expression)
    {
        end = vba::header_end(capture.node, cancellation)?;
        while end > start && source.get(end - 1).is_some_and(u8::is_ascii_whitespace) {
            end -= 1;
        }
    }
    if family == GrammarFamily::Perl && role == StructuralRole::Signature {
        end = perl::header_end(capture.node);
        while end > start && source.get(end - 1).is_some_and(u8::is_ascii_whitespace) {
            end -= 1;
        }
    }
    if family == GrammarFamily::Matlab && role == StructuralRole::Definition {
        start = matlab::definition_start(capture.node);
    }
    if family == GrammarFamily::Matlab && role == StructuralRole::Signature {
        end = matlab::header_end(capture.node);
        while end > start && source.get(end - 1).is_some_and(u8::is_ascii_whitespace) {
            end -= 1;
        }
    }
    if family == GrammarFamily::Nix && role == StructuralRole::Signature {
        let range = nix::signature_range(capture.node)
            .ok_or_else(|| query_failure("query-nix-signature"))?;
        start = range.start;
        end = range.end;
        while end > start && source.get(end - 1).is_some_and(u8::is_ascii_whitespace) {
            end -= 1;
        }
    }
    if family == GrammarFamily::ObjectiveC {
        let range = objective_c::capture_range(capture.node, role)
            .ok_or_else(|| query_failure("query-objective-c-range"))?;
        start = range.start;
        end = range.end;
    }
    if family == GrammarFamily::Dart && role == StructuralRole::Definition {
        let range = dart::definition_range(capture.node);
        start = range.start;
        end = range.end;
    }
    if family == GrammarFamily::PowerShell && role == StructuralRole::Signature {
        end = powershell::header_end(capture.node);
        while end > start && source.get(end - 1).is_some_and(u8::is_ascii_whitespace) {
            end -= 1;
        }
    }
    if family == GrammarFamily::Dart && role == StructuralRole::Signature {
        end = dart::header_end(capture.node);
        while end > start && source.get(end - 1).is_some_and(u8::is_ascii_whitespace) {
            end -= 1;
        }
    }
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
        && matches!(capture.node.kind(), "create_function" | "tsql_procedure")
    {
        let range = sql::header_signature_range(capture.node, source, cancellation)?
            .ok_or_else(|| query_failure("query-sql-signature-header"))?;
        start = range.start;
        end = range.end;
    }
    if family == GrammarFamily::Sql
        && role == StructuralRole::Declaration
        && let Some(header_end) = sql::recovered_header_end(capture.node, cancellation)?
    {
        // A recovered native body proves no complete callable extent.
        end = header_end;
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
    if family == GrammarFamily::Groovy
        && role == StructuralRole::Signature
        && matches!(capture.node.kind(), "closure" | "gstring_closure")
    {
        end = groovy::closure_header_end(capture.node, cancellation)?;
    }
    if matches!(
        family,
        GrammarFamily::Swift
            | GrammarFamily::Bash
            | GrammarFamily::Solidity
            | GrammarFamily::Scala
            | GrammarFamily::Groovy
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
    let mut capture_depth = if matches!(
        family,
        GrammarFamily::Perl
            | GrammarFamily::Groovy
            | GrammarFamily::Haskell
            | GrammarFamily::FSharp
            | GrammarFamily::FSharpSignature
            | GrammarFamily::CobolFixed
            | GrammarFamily::CobolFree
    ) || (family == GrammarFamily::Markdown
        && matches!(role, StructuralRole::Module | StructuralRole::Signature)
        && capture.node.kind() == "fenced_code_block")
    {
        // A sole fence can share the file's extent. Its actual native nesting
        // keeps the file outside the example instead of winning a lexical tie.
        native_depth(capture.node, cancellation)?
    } else {
        0
    };
    if matches!(family, GrammarFamily::CobolFixed | GrammarFamily::CobolFree)
        && role == StructuralRole::Scope
        && capture.node.kind() == "program_definition"
    {
        // A program's environment belongs beneath its same-node declaration.
        capture_depth = capture_depth
            .checked_add(1)
            .ok_or_else(|| query_failure("query-native-depth"))?;
    }
    if family == GrammarFamily::Haskell
        && role == StructuralRole::Scope
        && matches!(
            capture.node.kind(),
            "function"
                | "bind"
                | "data_type"
                | "newtype"
                | "type_synomym"
                | "class"
                | "data_constructor"
                | "newtype_constructor"
                | "gadt_constructor"
        )
        && haskell::retain_capture(capture.node, StructuralRole::Declaration, cancellation)?
    {
        // One native node owns both its binding and its lexical environment.
        // The environment must be inside that binding so repeated equations
        // share a callable identity without merging their formal parameters.
        capture_depth = capture_depth
            .checked_add(1)
            .ok_or_else(|| query_failure("query-native-depth"))?;
    }
    if matches!(
        family,
        GrammarFamily::FSharp | GrammarFamily::FSharpSignature
    ) && role == StructuralRole::Scope
        && fsharp::scope_below_declaration(capture.node)
    {
        // Same-span lexical environments belong below their named declaration.
        capture_depth = capture_depth
            .checked_add(1)
            .ok_or_else(|| query_failure("query-native-depth"))?;
    }
    Ok(QueryCandidate {
        start,
        end,
        role,
        syntax,
        required: false,
        native_depth: capture_depth,
    })
}

fn native_depth(
    mut node: tree_sitter::Node<'_>,
    cancellation: &Cancellation,
) -> Result<usize, AdapterError> {
    let mut depth = 0usize;
    while let Some(parent) = node.parent() {
        cancellation.check()?;
        depth = depth
            .checked_add(1)
            .ok_or_else(|| query_failure("query-native-depth"))?;
        node = parent;
    }
    Ok(depth)
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

fn is_php_named_class_constructor(method: tree_sitter::Node<'_>, source: &[u8]) -> bool {
    let Some(name) = method.child_by_field_name("name") else {
        return false;
    };
    if !source
        .get(name.byte_range())
        .is_some_and(|name| name.eq_ignore_ascii_case(b"__construct"))
    {
        return false;
    }
    let Some(body) = method
        .parent()
        .filter(|parent| parent.kind() == "declaration_list")
    else {
        return false;
    };
    // Anonymous owners have no retained named identity here; promoting their
    // methods would misattribute a Constructor to an outer named owner.
    body.parent().is_some_and(|owner| {
        owner.kind() == "class_declaration"
            && owner.child_by_field_name("name").is_some()
            && owner.child_by_field_name("body") == Some(body)
    })
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
        GrammarFamily::Apex
            | GrammarFamily::C
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
            | GrammarFamily::Dart
            | GrammarFamily::PowerShell
            | GrammarFamily::Nix
            | GrammarFamily::Groovy
            | GrammarFamily::Haskell
            | GrammarFamily::Fortran
    )
}

const fn supports_test_attribute(family: GrammarFamily) -> bool {
    matches!(family, GrammarFamily::Cpp | GrammarFamily::Java)
}

fn canonical_syntax(family: GrammarFamily, native: &str) -> Option<&'static str> {
    match (family, native) {
        (GrammarFamily::Idris, "idris") => Some("idris.file"),
        (GrammarFamily::Idris, "comment") => Some("idris.comment"),
        (GrammarFamily::Idris, "string" | "raw_string" | "triple_quote_string") => {
            Some("idris.string")
        }
        (GrammarFamily::Sed, "script") => Some("sed.file"),
        (GrammarFamily::Sed, "comment") => Some("sed.comment"),
        (GrammarFamily::Sed, "translation_string") => Some("sed.string"),
        (GrammarFamily::Markdown, "document") => Some("markdown.file"),
        (GrammarFamily::Markdown, "section") => Some("markdown.section"),
        (GrammarFamily::Markdown, "link_reference_definition") => Some("markdown.link_definition"),
        (GrammarFamily::Markdown, "inline") => Some("markdown.inline"),
        (GrammarFamily::Markdown, "link_label") => Some("markdown.link_label"),
        (GrammarFamily::Markdown, "block_continuation") => Some("markdown.continuation"),
        (GrammarFamily::Markdown, "fenced_code_block") => Some("markdown.code_block"),
        (GrammarFamily::Markdown, "paragraph") => Some("markdown.paragraph"),
        (GrammarFamily::Markdown, "atx_heading" | "setext_heading") => Some("markdown.heading"),
        (
            GrammarFamily::Markdown,
            "indented_code_block" | "html_block" | "minus_metadata" | "plus_metadata",
        ) => Some("markdown.embedded_text"),
        (GrammarFamily::Dart, native) => dart::canonical_syntax(native),
        (GrammarFamily::PowerShell, native) => powershell::canonical_syntax(native),
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
        (GrammarFamily::Sql, "tsql_procedure" | "tsql_procedure_header") => Some("sql.function"),
        (GrammarFamily::Sql, "tsql_parameter") => Some("sql.parameter"),
        (GrammarFamily::Sql, "tsql_variable" | "tsql_table_variable") => Some("sql.variable"),
        (GrammarFamily::Sql, "tsql_body") => Some("sql.body"),
        (GrammarFamily::Sql, "tsql_rejected_body") => Some("sql.unsupported_body"),
        (GrammarFamily::Sql, "function_arguments") => Some("sql.arguments"),
        (GrammarFamily::Sql, "function_body") => Some("sql.body"),
        (GrammarFamily::Sql, "function_argument") => Some("sql.parameter"),
        (GrammarFamily::Sql, "column_definition") => Some("sql.column"),
        (GrammarFamily::Sql, "object_reference" | "identifier") => Some("sql.identifier"),
        (GrammarFamily::Sql, "literal") => Some("sql.literal"),
        (GrammarFamily::Sql, "comment" | "marginalia") => Some("sql.comment"),
        (GrammarFamily::Html, "document") => Some("html.file"),
        (GrammarFamily::Astro, "document") => Some("astro.file"),
        (GrammarFamily::Astro, "element" | "script_element" | "style_element") => {
            Some("astro.element")
        }
        (GrammarFamily::Astro, "tag_name") => Some("astro.tag_name"),
        (GrammarFamily::Astro, "attribute") => Some("astro.attribute"),
        (GrammarFamily::Astro, "attribute_name") => Some("astro.attribute_name"),
        (GrammarFamily::Astro, "frontmatter_js_block") => Some("astro.frontmatter"),
        (GrammarFamily::Astro, "raw_text") => Some("astro.embedded_text"),
        (
            GrammarFamily::Astro,
            "html_interpolation" | "attribute_interpolation" | "attribute_backtick_string",
        ) => Some("astro.expression"),
        (GrammarFamily::Astro, "erroneous_end_tag") => Some("astro.unmatched_end_tag"),
        (GrammarFamily::Astro, "attribute_value" | "text") => Some("astro.text"),
        (GrammarFamily::Astro, "comment") => Some("astro.comment"),
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
        (GrammarFamily::Jsonc, "document") => Some("jsonc.file"),
        (GrammarFamily::Jsonc, "pair") => Some("jsonc.property"),
        (GrammarFamily::Jsonc, "object") => Some("jsonc.object"),
        (GrammarFamily::Jsonc, "array") => Some("jsonc.array"),
        (GrammarFamily::Jsonc, "string") => Some("jsonc.string"),
        (GrammarFamily::Jsonc, "comment") => Some("jsonc.comment"),
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
        (GrammarFamily::Python, "assignment") => Some("python.module_variable"),
        (GrammarFamily::Python, "augmented_assignment") => Some("python.augmented_assignment"),
        (GrammarFamily::Python, "function_definition") => Some("python.function"),
        (GrammarFamily::Python, "class_definition") => Some("python.class"),
        (GrammarFamily::Python, "import_statement") => Some("python.import"),
        (GrammarFamily::Python, "import_from_statement") => Some("python.import_from"),
        (GrammarFamily::Python, "parameters") => Some("python.parameters"),
        (
            GrammarFamily::Python,
            "typed_parameter"
            | "default_parameter"
            | "typed_default_parameter"
            | "list_splat_pattern"
            | "dictionary_splat_pattern",
        ) => Some("python.parameter"),
        (GrammarFamily::Python, "block") => Some("python.block"),
        (GrammarFamily::Python, "identifier") => Some("python.identifier"),
        (GrammarFamily::Python, "comment") => Some("python.comment"),
        (GrammarFamily::Python, "string") => Some("python.string"),
        (GrammarFamily::JavaScript, "program") => Some("javascript.program"),
        (GrammarFamily::JavaScript, "function_declaration") => Some("javascript.function"),
        (GrammarFamily::JavaScript, "class_declaration") => Some("javascript.class"),
        (GrammarFamily::JavaScript, "method_definition") => Some("javascript.method"),
        (GrammarFamily::JavaScript, "variable_declarator") => Some("javascript.variable"),
        (GrammarFamily::JavaScript, "import_statement") => Some("javascript.native_import"),
        (GrammarFamily::JavaScript, "formal_parameters") => Some("javascript.parameters"),
        (GrammarFamily::JavaScript, "statement_block") => Some("javascript.block"),
        (GrammarFamily::JavaScript, "for_statement" | "for_in_statement") => Some("javascript.for"),
        (GrammarFamily::JavaScript, "catch_clause") => Some("javascript.catch"),
        (GrammarFamily::JavaScript, "switch_body") => Some("javascript.switch"),
        (GrammarFamily::JavaScript, "class_static_block") => Some("javascript.static_block"),
        (
            GrammarFamily::JavaScript,
            "identifier"
            | "shorthand_property_identifier"
            | "shorthand_property_identifier_pattern",
        ) => Some("javascript.identifier"),
        (
            GrammarFamily::JavaScript,
            "function_expression"
            | "generator_function"
            | "arrow_function"
            | "function_type"
            | "constructor_type"
            | "call_signature"
            | "construct_signature",
        ) => Some("javascript.lambda"),
        (GrammarFamily::JavaScript, "generator_function_declaration" | "function_signature") => {
            Some("javascript.function")
        }
        (GrammarFamily::JavaScript, "method_signature" | "abstract_method_signature") => {
            Some("javascript.method")
        }
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
        (GrammarFamily::Java, "formal_parameter") => Some("java.parameter"),
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
        (GrammarFamily::TypeScript, "property_signature") => Some("typescript.field"),
        (GrammarFamily::TypeScript, "type_alias_declaration") => Some("typescript.type_alias"),
        (GrammarFamily::TypeScript, "type_parameter") => Some("typescript.type_parameter"),
        (GrammarFamily::TypeScript, "enum_declaration") => Some("typescript.enum"),
        (GrammarFamily::TypeScript, "method_definition") => Some("typescript.method"),
        (GrammarFamily::TypeScript, "method_signature") => Some("typescript.method_signature"),
        (GrammarFamily::TypeScript, "abstract_method_signature") => {
            Some("typescript.abstract_method")
        }
        (GrammarFamily::TypeScript, "variable_declarator") => Some("typescript.variable"),
        (GrammarFamily::TypeScript, "import_statement") => Some("typescript.native_import"),
        (GrammarFamily::TypeScript, "import_alias") => Some("typescript.import"),
        (GrammarFamily::TypeScript, "formal_parameters") => Some("typescript.parameters"),
        (GrammarFamily::TypeScript, "statement_block") => Some("typescript.block"),
        (GrammarFamily::TypeScript, "for_statement" | "for_in_statement") => Some("typescript.for"),
        (GrammarFamily::TypeScript, "catch_clause") => Some("typescript.catch"),
        (GrammarFamily::TypeScript, "switch_body") => Some("typescript.switch"),
        (GrammarFamily::TypeScript, "class_static_block") => Some("typescript.static_block"),
        (
            GrammarFamily::TypeScript,
            "identifier"
            | "shorthand_property_identifier"
            | "shorthand_property_identifier_pattern",
        ) => Some("typescript.identifier"),
        (
            GrammarFamily::TypeScript,
            "function_expression"
            | "generator_function"
            | "arrow_function"
            | "function_type"
            | "constructor_type"
            | "call_signature"
            | "construct_signature",
        ) => Some("typescript.lambda"),
        (GrammarFamily::TypeScript, "generator_function_declaration") => {
            Some("typescript.function")
        }
        (GrammarFamily::TypeScript, "mapped_type_clause") => {
            Some("typescript.mapped_type_parameter")
        }
        (GrammarFamily::TypeScript, "index_signature") => Some("typescript.mapped_type"),
        (GrammarFamily::TypeScript, "infer_type") => Some("typescript.infer_parameter"),
        (GrammarFamily::TypeScript, "conditional_type") => Some("typescript.conditional"),
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
        (GrammarFamily::C, "init_declarator") => Some("c.variable"),
        (GrammarFamily::C, "for_statement") => Some("c.for"),
        (GrammarFamily::C, "struct_specifier") => Some("c.struct"),
        (GrammarFamily::C, "union_specifier") => Some("c.union"),
        (GrammarFamily::C, "enum_specifier") => Some("c.enum"),
        (GrammarFamily::C, "type_definition") => Some("c.type"),
        (GrammarFamily::C, "preproc_def") => Some("c.macro"),
        (GrammarFamily::C, "preproc_function_def") => Some("c.function_macro"),
        (GrammarFamily::C, "preproc_if" | "preproc_ifdef") => Some("c.preprocessor_condition"),
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
        (GrammarFamily::Cpp, "preproc_def") => Some("cpp.macro"),
        (GrammarFamily::Cpp, "preproc_function_def") => Some("cpp.function_macro"),
        (GrammarFamily::Cpp, "preproc_params") => Some("cpp.preprocessor_parameters"),
        (GrammarFamily::Cpp, "preproc_if" | "preproc_ifdef") => Some("cpp.preprocessor_condition"),
        (GrammarFamily::Cpp, "parameter_declaration") => Some("cpp.const_reference_parameter"),
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
        (GrammarFamily::CSharp, "parameter") => Some("csharp.parameter"),
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
    #[cfg(test)]
    pub(crate) fn uninitialized_for_test() -> Self {
        Self::catalogue()
    }

    pub(crate) fn audited() -> Result<&'static Self, GrammarFamily> {
        let registry = Self::deferred();
        let cancellation = Cancellation::new();
        for (family, entry) in &registry.packs {
            entry.load(&cancellation).map_err(|_| *family)?;
        }
        registry
            .typescript_tsx
            .load(&cancellation)
            .map_err(|_| GrammarFamily::TypeScript)?;
        Ok(registry)
    }

    pub(crate) fn deferred() -> &'static Self {
        // Native query analysis is expensive (see benches/provider_startup.rs).
        // A fresh isolated request must not compile unrelated languages. Only
        // immutable built-in queries and their deterministic failures are shared;
        // source trees, cursors and caller budgets remain provider-owned.
        static REGISTRY: OnceLock<QueryPackRegistry> = OnceLock::new();
        REGISTRY.get_or_init(Self::catalogue)
    }

    fn catalogue() -> Self {
        let mut packs = Vec::with_capacity(81);
        for (family, source) in [
            (
                GrammarFamily::Clojure,
                include_str!("../queries/clojure.scm"),
            ),
            (GrammarFamily::Racket, include_str!("../queries/racket.scm")),
            (
                GrammarFamily::Graphql,
                include_str!("../queries/graphql.scm"),
            ),
            (GrammarFamily::Elixir, include_str!("../queries/elixir.scm")),
            (GrammarFamily::Capnp, include_str!("../queries/capnp.scm")),
            (GrammarFamily::Rust, include_str!("../queries/rust.scm")),
            (GrammarFamily::Python, include_str!("../queries/python.scm")),
            (
                GrammarFamily::JavaScript,
                concat!(
                    include_str!("../queries/javascript.scm"),
                    "\n",
                    include_str!("../queries/jsx.scm")
                ),
            ),
            (GrammarFamily::Java, include_str!("../queries/java.scm")),
            (GrammarFamily::Apex, include_str!("../queries/apex.scm")),
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
            (GrammarFamily::Jsonc, include_str!("../queries/json.scm")),
            (
                GrammarFamily::JsonSchema,
                include_str!("../queries/json_schema.scm"),
            ),
            (GrammarFamily::Toml, include_str!("../queries/toml.scm")),
            (
                GrammarFamily::Protobuf,
                include_str!("../queries/protobuf.scm"),
            ),
            (GrammarFamily::Thrift, include_str!("../queries/thrift.scm")),
            (GrammarFamily::Yaml, include_str!("../queries/yaml.scm")),
            (GrammarFamily::Html, include_str!("../queries/html.scm")),
            (GrammarFamily::Astro, include_str!("../queries/astro.scm")),
            (
                GrammarFamily::ObjectiveC,
                include_str!("../queries/objective_c.scm"),
            ),
            (
                GrammarFamily::Markdown,
                include_str!("../queries/markdown.scm"),
            ),
            (GrammarFamily::Sql, include_str!("../queries/sql.scm")),
            (GrammarFamily::R, include_str!("../queries/r.scm")),
            (GrammarFamily::Nix, include_str!("../queries/nix.scm")),
            (GrammarFamily::Matlab, include_str!("../queries/matlab.scm")),
            (GrammarFamily::Perl, include_str!("../queries/perl.scm")),
            (GrammarFamily::Groovy, include_str!("../queries/groovy.scm")),
            (
                GrammarFamily::Haskell,
                include_str!("../queries/haskell.scm"),
            ),
            (GrammarFamily::FSharp, include_str!("../queries/fsharp.scm")),
            (
                GrammarFamily::Fortran,
                include_str!("../queries/fortran.scm"),
            ),
            (GrammarFamily::PlSql, include_str!("../queries/plsql.scm")),
            (GrammarFamily::Sas, include_str!("../queries/sas.scm")),
            (GrammarFamily::Glsl, include_str!("../queries/glsl.scm")),
            (GrammarFamily::Hlsl, include_str!("../queries/hlsl.scm")),
            (GrammarFamily::Wgsl, include_str!("../queries/wgsl.scm")),
            (GrammarFamily::Xml, include_str!("../queries/xml.scm")),
            (GrammarFamily::Dtd, include_str!("../queries/dtd.scm")),
            (GrammarFamily::CMake, include_str!("../queries/cmake.scm")),
            (GrammarFamily::Coq, include_str!("../queries/coq.scm")),
            (GrammarFamily::Julia, include_str!("../queries/julia.scm")),
            (GrammarFamily::Eiffel, include_str!("../queries/eiffel.scm")),
            (GrammarFamily::V, include_str!("../queries/v.scm")),
            (GrammarFamily::Vhdl, include_str!("../queries/vhdl.scm")),
            (GrammarFamily::Ada, include_str!("../queries/ada.scm")),
            (GrammarFamily::Zig, include_str!("../queries/zig.scm")),
            (GrammarFamily::Hcl, include_str!("../queries/hcl.scm")),
            (
                GrammarFamily::StandardMl,
                include_str!("../queries/standard_ml.scm"),
            ),
            (
                GrammarFamily::Wat,
                include_str!("../queries/webassembly.scm"),
            ),
            (
                GrammarFamily::Dockerfile,
                include_str!("../queries/dockerfile.scm"),
            ),
            (
                GrammarFamily::Coffeescript,
                include_str!("../queries/coffeescript.scm"),
            ),
            (
                GrammarFamily::Wolfram,
                include_str!("../queries/wolfram.scm"),
            ),
            (GrammarFamily::Idris, include_str!("../queries/idris.scm")),
            (GrammarFamily::Sed, include_str!("../queries/sed.scm")),
            (GrammarFamily::Pug, include_str!("../queries/pug.scm")),
            (GrammarFamily::Lean, include_str!("../queries/lean.scm")),
            (GrammarFamily::Meson, include_str!("../queries/meson.scm")),
            (GrammarFamily::Chapel, include_str!("../queries/chapel.scm")),
            (
                GrammarFamily::Gdscript,
                include_str!("../queries/gdscript.scm"),
            ),
            (GrammarFamily::Qsharp, include_str!("../queries/qsharp.scm")),
            (GrammarFamily::Mojo, include_str!("../queries/mojo.scm")),
            (GrammarFamily::Haxe, include_str!("../queries/haxe.scm")),
            (GrammarFamily::Qml, include_str!("../queries/qml.scm")),
            (GrammarFamily::Ring, include_str!("../queries/ring.scm")),
            (GrammarFamily::Hare, include_str!("../queries/hare.scm")),
            (
                GrammarFamily::Modelica,
                include_str!("../queries/modelica.scm"),
            ),
            (GrammarFamily::D, include_str!("../queries/d.scm")),
            (GrammarFamily::Cue, include_str!("../queries/cue.scm")),
            (
                GrammarFamily::Crystal,
                include_str!("../queries/crystal.scm"),
            ),
            (GrammarFamily::Pony, include_str!("../queries/pony.scm")),
            (GrammarFamily::Fish, include_str!("../queries/fish.scm")),
            (GrammarFamily::Nim, include_str!("../queries/nim.scm")),
            (GrammarFamily::Verilog, include_str!("../queries/hdl.scm")),
            (
                GrammarFamily::SystemVerilog,
                include_str!("../queries/hdl.scm"),
            ),
            (GrammarFamily::Pascal, include_str!("../queries/pascal.scm")),
            (
                GrammarFamily::Assembly,
                include_str!("../queries/assembly.scm"),
            ),
            (
                GrammarFamily::CommonLisp,
                include_str!("../queries/common_lisp.scm"),
            ),
            (GrammarFamily::Scheme, include_str!("../queries/scheme.scm")),
            (GrammarFamily::Ocaml, include_str!("../queries/ocaml.scm")),
            (
                GrammarFamily::OcamlInterface,
                include_str!("../queries/ocaml-interface.scm"),
            ),
            (
                GrammarFamily::FSharpSignature,
                include_str!("../queries/fsharp_signature.scm"),
            ),
            (
                GrammarFamily::CobolFixed,
                include_str!("../queries/cobol.scm"),
            ),
            (
                GrammarFamily::CobolFree,
                include_str!("../queries/cobol.scm"),
            ),
            (GrammarFamily::Vba, include_str!("../queries/vba.scm")),
            (
                GrammarFamily::VisualBasicClassic,
                include_str!("../queries/vba.scm"),
            ),
            (GrammarFamily::Scala, include_str!("../queries/scala.scm")),
            (GrammarFamily::Dart, include_str!("../queries/dart.scm")),
            (
                GrammarFamily::PowerShell,
                include_str!("../queries/powershell.scm"),
            ),
            (
                GrammarFamily::Solidity,
                include_str!("../queries/solidity.scm"),
            ),
        ] {
            packs.push((family, QueryPackEntry::new(family, family, source)));
        }
        packs.sort_by_key(|(family, _)| *family);
        let typescript_tsx = QueryPackEntry::new(
            GrammarFamily::TypeScript,
            GrammarFamily::JavaScript,
            concat!(
                include_str!("../queries/typescript.scm"),
                "\n",
                include_str!("../queries/jsx.scm")
            ),
        );
        Self {
            packs,
            typescript_tsx,
        }
    }

    fn entry(&self, family: GrammarFamily) -> Option<&QueryPackEntry> {
        self.packs
            .binary_search_by_key(&family, |(registered, _)| *registered)
            .ok()
            .and_then(|index| self.packs.get(index))
            .map(|(_, pack)| pack)
    }

    pub(crate) fn contains(&self, family: GrammarFamily) -> bool {
        self.entry(family).is_some()
    }

    #[cfg(test)]
    pub(crate) fn get(&self, family: GrammarFamily) -> Result<&QueryPack, QueryPackError> {
        self.get_for_native(family, family, &Cancellation::new())
    }

    pub(crate) const fn len(&self) -> usize {
        self.packs.len() + 1
    }

    pub(crate) fn get_for_source(
        &self,
        family: GrammarFamily,
        path: &str,
        cancellation: &Cancellation,
    ) -> Result<&QueryPack, QueryPackError> {
        self.get_for_native(family, native_family_for_source(family, path), cancellation)
    }

    pub(crate) fn get_for_native(
        &self,
        family: GrammarFamily,
        native: GrammarFamily,
        cancellation: &Cancellation,
    ) -> Result<&QueryPack, QueryPackError> {
        cancellation.check()?;
        let entry = if family == GrammarFamily::TypeScript && native == GrammarFamily::JavaScript {
            Some(&self.typescript_tsx)
        } else if family == native {
            self.entry(family)
        } else {
            None
        };
        entry
            .ok_or(QueryPackError::Missing { family, native })?
            .load(cancellation)
    }

    pub(crate) fn initialized_count(&self) -> usize {
        self.packs
            .iter()
            .map(|(_, entry)| entry)
            .chain(std::iter::once(&self.typescript_tsx))
            .filter(|entry| entry.compiled.get().is_some())
            .count()
    }

    pub(crate) fn initialized_pattern_count(&self) -> usize {
        self.packs
            .iter()
            .map(|(_, entry)| entry)
            .chain(std::iter::once(&self.typescript_tsx))
            .filter_map(|entry| entry.compiled.get().and_then(|result| result.as_ref().ok()))
            .map(|pack| pack.identity_query.pattern_count())
            .sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pascal_pack_compiles_with_only_written_source_captures() {
        let pack = QueryPack::compile(GrammarFamily::Pascal, include_str!("../queries/pascal.scm"))
            .expect("reviewed Pascal query matches the linked static grammar");
        let mut names = pack.identity_query.capture_names().to_vec();
        names.sort_unstable();
        assert_eq!(
            names,
            [
                "comment",
                "declaration",
                "definition",
                "expression",
                "module",
                "reference",
                "root",
                "scope",
                "signature",
                "string",
            ]
        );
        assert!(pack.roles_by_capture.iter().all(|role| !matches!(
            role,
            StructuralRole::Call | StructuralRole::CallName | StructuralRole::ScopedCall
        )));
    }

    #[test]
    fn reviewed_packs_compile_with_the_exact_closed_capture_contract() {
        let registry = QueryPackRegistry::audited().expect("reviewed packs compile");
        assert_eq!(registry.len(), 101);
        assert_eq!(registry.initialized_count(), registry.len());

        for family in [
            GrammarFamily::Clojure,
            GrammarFamily::Racket,
            GrammarFamily::Rust,
            GrammarFamily::Python,
            GrammarFamily::JavaScript,
            GrammarFamily::Java,
            GrammarFamily::Apex,
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
            GrammarFamily::Jsonc,
            GrammarFamily::JsonSchema,
            GrammarFamily::Toml,
            GrammarFamily::Yaml,
            GrammarFamily::Html,
            GrammarFamily::Sql,
            GrammarFamily::R,
            GrammarFamily::Solidity,
            GrammarFamily::Scala,
            GrammarFamily::Dart,
            GrammarFamily::PowerShell,
            GrammarFamily::Markdown,
            GrammarFamily::Astro,
            GrammarFamily::ObjectiveC,
            GrammarFamily::Nix,
            GrammarFamily::Matlab,
            GrammarFamily::Perl,
            GrammarFamily::Groovy,
            GrammarFamily::Vba,
            GrammarFamily::VisualBasicClassic,
            GrammarFamily::Haskell,
            GrammarFamily::FSharp,
            GrammarFamily::FSharpSignature,
            GrammarFamily::CobolFixed,
            GrammarFamily::CobolFree,
            GrammarFamily::Fortran,
            GrammarFamily::PlSql,
            GrammarFamily::Sas,
            GrammarFamily::Pascal,
            GrammarFamily::CommonLisp,
            GrammarFamily::Scheme,
            GrammarFamily::Ocaml,
            GrammarFamily::OcamlInterface,
            GrammarFamily::Assembly,
            GrammarFamily::Glsl,
            GrammarFamily::Hlsl,
            GrammarFamily::Protobuf,
            GrammarFamily::Thrift,
            GrammarFamily::Wgsl,
            GrammarFamily::Xml,
            GrammarFamily::Dtd,
            GrammarFamily::CMake,
            GrammarFamily::Julia,
            GrammarFamily::Eiffel,
            GrammarFamily::Coq,
            GrammarFamily::V,
            GrammarFamily::Verilog,
            GrammarFamily::Vhdl,
            GrammarFamily::Hcl,
            GrammarFamily::Ada,
            GrammarFamily::Dockerfile,
            GrammarFamily::Coffeescript,
            GrammarFamily::Idris,
            GrammarFamily::Sed,
            GrammarFamily::Wolfram,
            GrammarFamily::Pug,
            GrammarFamily::Lean,
            GrammarFamily::Meson,
            GrammarFamily::Gdscript,
            GrammarFamily::Qsharp,
            GrammarFamily::Chapel,
            GrammarFamily::StandardMl,
            GrammarFamily::Wat,
            GrammarFamily::Mojo,
            GrammarFamily::Haxe,
            GrammarFamily::Qml,
            GrammarFamily::Modelica,
            GrammarFamily::Ring,
            GrammarFamily::Hare,
            GrammarFamily::D,
            GrammarFamily::Hcl,
            GrammarFamily::Cue,
            GrammarFamily::Crystal,
            GrammarFamily::Pony,
            GrammarFamily::Fish,
            GrammarFamily::Nim,
            GrammarFamily::SystemVerilog,
        ] {
            let pack = registry.get(family).expect("family has a query pack");
            let mut names = pack.identity_query.capture_names().to_vec();
            names.sort_unstable();
            let mut expected = EXPECTED_CAPTURES.to_vec();
            if family == GrammarFamily::Idris {
                expected.retain(|name| !matches!(*name, "call" | "import" | "documentation"));
            }
            if family == GrammarFamily::Sed {
                expected.retain(|name| {
                    matches!(
                        *name,
                        "root"
                            | "module"
                            | "declaration"
                            | "definition"
                            | "reference"
                            | "signature"
                            | "comment"
                            | "string"
                    )
                });
            }
            if matches!(family, GrammarFamily::Wolfram | GrammarFamily::Lean) {
                expected.retain(|name| !matches!(*name, "call" | "import" | "documentation"));
            }
            if family == GrammarFamily::Coq {
                expected
                    .retain(|name| !matches!(*name, "call" | "import" | "scope" | "documentation"));
            }
            if matches!(family, GrammarFamily::Ocaml | GrammarFamily::OcamlInterface) {
                expected.retain(|name| *name != "import");
                if family == GrammarFamily::OcamlInterface {
                    expected.retain(|name| !matches!(*name, "call" | "reference"));
                }
                expected.push("expression");
            }
            if matches!(family, GrammarFamily::CobolFixed | GrammarFamily::CobolFree) {
                expected.retain(|name| !matches!(*name, "call" | "module" | "documentation"));
            }
            if family == GrammarFamily::Perl {
                expected.retain(|name| !matches!(*name, "call" | "import"));
            }
            if family == GrammarFamily::Fortran {
                expected.retain(|name| *name != "documentation");
                expected.push("expression");
            }
            if family == GrammarFamily::Dockerfile {
                expected.retain(|name| {
                    !matches!(
                        *name,
                        "call" | "import" | "documentation" | "module" | "scope" | "string"
                    )
                });
            }
            if family == GrammarFamily::Meson {
                expected.retain(|name| {
                    !matches!(
                        *name,
                        "call" | "import" | "documentation" | "module" | "scope"
                    )
                });
            }
            if family == GrammarFamily::Protobuf {
                expected.retain(|name| !matches!(*name, "call" | "documentation"));
                expected.push("expression");
            }

            if family == GrammarFamily::Thrift {
                expected.retain(|name| !matches!(*name, "call" | "documentation"));
            }

            if family == GrammarFamily::Wgsl {
                expected.retain(|name| {
                    !matches!(
                        *name,
                        "call" | "comment" | "documentation" | "import" | "string"
                    )
                });
                expected.push("expression");
            }
            if family == GrammarFamily::Apex {
                expected.retain(|name| *name != "import");
                expected.push("expression");
            }
            if family == GrammarFamily::Assembly {
                expected.retain(|name| {
                    !matches!(*name, "call" | "import" | "documentation" | "reference")
                });
                expected.push("expression");
            }
            if matches!(
                family,
                GrammarFamily::PlSql
                    | GrammarFamily::Sas
                    | GrammarFamily::Pascal
                    | GrammarFamily::Clojure
                    | GrammarFamily::Racket
                    | GrammarFamily::CommonLisp
                    | GrammarFamily::Scheme
                    | GrammarFamily::Glsl
                    | GrammarFamily::Hlsl
            ) {
                expected.retain(|name| !matches!(*name, "call" | "import" | "documentation"));
                expected.push("expression");
            }
            if matches!(
                family,
                GrammarFamily::FSharp | GrammarFamily::FSharpSignature
            ) {
                expected.retain(|name| {
                    !matches!(
                        *name,
                        "call" | "comment" | "documentation" | "reference" | "string"
                    )
                });
            }
            if family == GrammarFamily::FSharp {
                expected.extend(["call", "expression", "reference"]);
            }
            if family == GrammarFamily::FSharpSignature {
                expected.push("expression");
            }
            if family == GrammarFamily::Matlab || family.is_classic_basic() {
                expected.retain(|name| !matches!(*name, "call" | "import" | "documentation"));
            }
            if family.is_classic_basic() {
                expected.push("expression");
            }
            if family == GrammarFamily::Markdown {
                expected.retain(|name| {
                    !matches!(
                        *name,
                        "call" | "import" | "reference" | "scope" | "comment" | "documentation"
                    )
                });
            }
            if family == GrammarFamily::Pug {
                expected.retain(|name| {
                    !matches!(
                        *name,
                        "call" | "import" | "reference" | "scope" | "documentation"
                    )
                });
            }
            if family == GrammarFamily::Yaml {
                expected.retain(|name| !matches!(*name, "call" | "import"));
            }
            if family == GrammarFamily::Xml {
                expected.retain(|name| !matches!(*name, "call" | "import" | "scope"));
            }
            if matches!(
                family,
                GrammarFamily::Eiffel | GrammarFamily::Verilog | GrammarFamily::SystemVerilog
            ) {
                expected.retain(|name| !matches!(*name, "import" | "scope" | "documentation"));
            }
            if matches!(
                family,
                GrammarFamily::CMake | GrammarFamily::Julia | GrammarFamily::V
            ) {
                expected.retain(|name| !matches!(*name, "scope" | "documentation"));
            }
            if family == GrammarFamily::Vhdl {
                expected
                    .retain(|name| !matches!(*name, "call" | "import" | "scope" | "documentation"));
            }
            if matches!(family, GrammarFamily::Cue | GrammarFamily::Hcl) {
                expected.retain(|name| {
                    !matches!(
                        *name,
                        "call" | "import" | "reference" | "documentation" | "signature"
                    )
                });
                expected.push("expression");
            }
            if matches!(
                family,
                GrammarFamily::Pony
                    | GrammarFamily::Crystal
                    | GrammarFamily::Zig
                    | GrammarFamily::D
            ) {
                expected.retain(|name| {
                    !matches!(*name, "call" | "import" | "reference" | "documentation")
                });
                expected.push("expression");
            }
            if family == GrammarFamily::Qml {
                expected.retain(|name| !matches!(*name, "reference" | "import" | "documentation"));
                expected.push("expression");
            }
            if matches!(
                family,
                GrammarFamily::Coffeescript
                    | GrammarFamily::Gdscript
                    | GrammarFamily::Chapel
                    | GrammarFamily::Qsharp
                    | GrammarFamily::StandardMl
                    | GrammarFamily::Wat
                    | GrammarFamily::Mojo
                    | GrammarFamily::Haxe
                    | GrammarFamily::Modelica
                    | GrammarFamily::Ring
                    | GrammarFamily::Hare
            ) {
                expected.retain(|name| !matches!(*name, "call" | "import" | "documentation"));
                expected.push("expression");
            }
            if family == GrammarFamily::Wat {
                expected.retain(|name| *name != "module");
            }
            if family == GrammarFamily::Ada {
                expected.retain(|name| !matches!(*name, "call" | "import" | "documentation"));
                expected.push("expression");
            }
            if family == GrammarFamily::Fish {
                expected.retain(|name| {
                    !matches!(*name, "call" | "import" | "reference" | "documentation")
                });
                expected.push("expression");
            }
            if matches!(
                family,
                GrammarFamily::JavaScript | GrammarFamily::Scala | GrammarFamily::PowerShell
            ) {
                expected.push("expression");
            }
            if family == GrammarFamily::Nim {
                expected.retain(|name| !matches!(*name, "call" | "import" | "documentation"));
                expected.push("expression");
            }
            if family == GrammarFamily::Dtd {
                expected.retain(|name| {
                    !matches!(
                        *name,
                        "call" | "import" | "scope" | "declaration" | "definition"
                    )
                });
            }
            if matches!(family, GrammarFamily::Html | GrammarFamily::Astro) {
                expected.retain(|name| !matches!(*name, "call" | "import" | "reference" | "scope"));
            }
            if family == GrammarFamily::Sql {
                expected.retain(|name| !matches!(*name, "call" | "import" | "scope"));
            }
            if family == GrammarFamily::JsonSchema {
                expected.retain(|name| {
                    !matches!(*name, "call" | "import" | "comment" | "documentation")
                });
                expected.push("expression");
            }
            if family.is_json() || family == GrammarFamily::Toml {
                expected
                    .retain(|name| !matches!(*name, "call" | "reference" | "signature" | "import"));
            }
            if matches!(
                family,
                GrammarFamily::Lua
                    | GrammarFamily::Ruby
                    | GrammarFamily::Bash
                    | GrammarFamily::R
                    | GrammarFamily::PowerShell
                    | GrammarFamily::Nix
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
                if family == GrammarFamily::ObjectiveC {
                    expected.extend(["definition_part", "scope_trait", "scope_type"]);
                }
                if family == GrammarFamily::Nix {
                    expected.extend(["definition_part", "expression"]);
                }
                if matches!(
                    family,
                    GrammarFamily::Perl | GrammarFamily::Groovy | GrammarFamily::Sql
                ) {
                    expected.push("expression");
                }
                if supports_terminal_call_name(family) {
                    expected.push(TERMINAL_CALL_NAME_CAPTURE);
                }
                if supports_test_attribute(family) {
                    expected.push("test_attribute");
                }
                expected.sort_unstable();
            }
            assert_eq!(names, expected, "capture contract differs for {family:?}");
            assert_eq!(pack.roles_by_capture.len(), expected.len());
        }
    }

    fn dockerfile_tree(source: &[u8], partial_end: Option<usize>) -> tree_sitter::Tree {
        let scope = tree_sitter_dockerfile::ScannerScope::begin((), |_| false).unwrap();
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&language_for(GrammarFamily::Dockerfile))
            .unwrap();
        if let Some(end) = partial_end {
            parser
                .set_included_ranges(&[tree_sitter::Range {
                    start_byte: 0,
                    end_byte: end,
                    start_point: tree_sitter::Point::new(0, 0),
                    end_point: tree_sitter::Point::new(1, 0),
                }])
                .unwrap();
        }
        let tree = parser.parse(source, None).unwrap();
        drop(parser);
        scope.finish().unwrap();
        tree
    }

    #[test]
    fn dockerfile_required_copy_reference_survives_zero_optional_credit_exactly_once() {
        let source = b"FROM scratch AS base\nFROM scratch AS final\nCOPY --from=base /a /b\n";
        let tree = dockerfile_tree(source, None);
        assert!(!tree.root_node().has_error());
        let pack = QueryPackRegistry::audited()
            .unwrap()
            .get(GrammarFamily::Dockerfile)
            .unwrap();
        let identity = pack
            .extract_identity(
                GrammarFamily::Dockerfile,
                &tree,
                source,
                10000,
                &Cancellation::new(),
            )
            .unwrap();
        let copies: Vec<_> = identity
            .iter()
            .filter(|candidate| candidate.syntax == "dockerfile.copy_from")
            .collect();
        assert_eq!(copies.len(), 1);
        assert_eq!(copies[0].role, StructuralRole::Reference);
        assert!(copies[0].required);
        assert_eq!(
            source.get(copies[0].start..copies[0].end),
            Some(b"base".as_slice())
        );
        let zero = pack
            .extract(
                GrammarFamily::Dockerfile,
                &tree,
                source,
                10000,
                0,
                &Cancellation::new(),
            )
            .unwrap();
        let full = pack
            .extract(
                GrammarFamily::Dockerfile,
                &tree,
                source,
                10000,
                10000,
                &Cancellation::new(),
            )
            .unwrap();
        assert_eq!(
            zero.candidates
                .iter()
                .filter(|candidate| candidate.required)
                .count(),
            identity.len()
        );
        assert_eq!(
            full.candidates
                .iter()
                .filter(|candidate| candidate.syntax == "dockerfile.copy_from")
                .count(),
            1
        );
        assert!(
            zero.candidates
                .iter()
                .any(|candidate| candidate.syntax == "dockerfile.copy_from" && candidate.required)
        );
        let mut names = pack.identity_query.capture_names().to_vec();
        names.sort_unstable();
        assert_eq!(
            names,
            [
                "comment",
                "declaration",
                "definition",
                "reference",
                "root",
                "signature"
            ]
        );
    }

    #[test]
    fn dockerfile_partial_native_input_cannot_certify_stage_or_copy_identity() {
        let source = b"FROM scratch AS base\nFROM scratch AS final\nCOPY --from=base /a /b\n";
        let first_line_end = source.iter().position(|byte| *byte == b'\n').unwrap() + 1;
        let tree = dockerfile_tree(source, Some(first_line_end));
        let pack = QueryPackRegistry::audited()
            .unwrap()
            .get(GrammarFamily::Dockerfile)
            .unwrap();
        let identity = pack
            .extract_identity(
                GrammarFamily::Dockerfile,
                &tree,
                source,
                10000,
                &Cancellation::new(),
            )
            .unwrap();
        assert!(!identity.iter().any(|candidate| matches!(
            candidate.syntax,
            "dockerfile.stage" | "dockerfile.stage_alias" | "dockerfile.copy_from"
        )));
    }

    #[test]
    fn dockerfile_native_helper_refusal_retains_real_origin_without_partial_identities() {
        let source = b"FROM scratch AS base\nFROM scratch AS final\nCOPY --from=base /a /b\n";
        let tree = dockerfile_tree(source, None);
        let pack = QueryPackRegistry::audited()
            .unwrap()
            .get(GrammarFamily::Dockerfile)
            .unwrap();
        let cancellation = Cancellation::new();
        let mut issued = None;
        let mut emitted = Vec::new();
        let limit = pack
            .scan_query_with_refusal(
                &pack.identity_query,
                QueryInput {
                    family: GrammarFamily::Dockerfile,
                    tree: &tree,
                    source,
                    cancellation: &cancellation,
                    normalize_calls: false,
                },
                QueryScanLimits {
                    matches: 100,
                    captures: 100,
                    body_nodes: 0,
                },
                |candidate| {
                    emitted.push(candidate);
                    Ok(())
                },
                Some(&mut issued),
            )
            .unwrap();
        assert_eq!(limit, Some(QueryLimit::Capture));
        assert!(emitted.is_empty());
        let event = issued.unwrap();
        assert_eq!(
            event.details().origin(),
            "query-identity-dockerfile-captures-limit"
        );
        assert_eq!(
            (event.details().observed(), event.details().limit()),
            (None, None)
        );
        assert!(event.matches_error(&event.error()));
    }

    #[test]
    fn rust_complete_proof_context_keeps_the_same_bom_extent_as_its_root() {
        let source = "\u{feff}// λ 😀\nfn helper() {} fn run() { helper(); }\n".as_bytes();
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&language_for(GrammarFamily::Rust))
            .unwrap();
        let tree = parser.parse(source, None).unwrap();
        assert!(!tree.root_node().has_error());
        assert_eq!(tree.root_node().start_byte(), 3);
        let registry = QueryPackRegistry::audited().unwrap();
        let extraction = registry
            .get(GrammarFamily::Rust)
            .unwrap()
            .extract(
                GrammarFamily::Rust,
                &tree,
                source,
                usize::MAX,
                usize::MAX,
                &Cancellation::new(),
            )
            .unwrap();
        for (role, syntax) in [
            (StructuralRole::Root, "rust.file"),
            (StructuralRole::Signature, "rust.proof_context"),
        ] {
            let candidates: Vec<_> = extraction
                .candidates
                .iter()
                .filter(|candidate| candidate.role == role && candidate.syntax == syntax)
                .collect();
            assert_eq!(candidates.len(), 1);
            assert_eq!(candidates[0].start..candidates[0].end, 0..source.len());
        }
        assert!(
            extraction.candidates.iter().any(|candidate| {
                candidate.role == StructuralRole::Signature
                    && candidate.syntax != "rust.proof_context"
                    && candidate.start > 3
            }),
            "only the whole-file proof context owns the BOM"
        );
    }

    #[test]
    fn optional_fact_credit_is_not_an_allocation_or_arithmetic_request() {
        let source = b"fn example() { first(); second(); }\n";
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&language_for(GrammarFamily::Rust))
            .unwrap();
        let tree = parser.parse(source, None).unwrap();
        let registry = QueryPackRegistry::audited().unwrap();
        let pack = registry.get(GrammarFamily::Rust).unwrap();
        let extract = |credit| {
            pack.extract(
                GrammarFamily::Rust,
                &tree,
                source,
                usize::MAX,
                credit,
                &Cancellation::new(),
            )
            .unwrap()
        };
        let unrestricted = extract(usize::MAX);
        assert_eq!(unrestricted.limit, None);
        assert_eq!(unrestricted.fact_limit, usize::MAX);
        assert!(
            unrestricted
                .candidates
                .iter()
                .filter(|candidate| !candidate.required)
                .count()
                > 1
        );
        let limited = extract(1);
        assert_eq!(limited.limit, Some(QueryLimit::Fact));
        let identity_only = extract(0);
        assert_eq!(limited.candidates.len(), identity_only.candidates.len() + 1);
        assert!(limited.candidates.starts_with(&identity_only.candidates));
        assert!(
            unrestricted
                .candidates
                .starts_with(&identity_only.candidates)
        );
        assert_eq!(
            limited
                .candidates
                .iter()
                .filter(|candidate| candidate.required)
                .count(),
            unrestricted
                .candidates
                .iter()
                .filter(|candidate| candidate.required)
                .count()
        );
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

    #[test]
    fn vba_compilation_headers_survive_optional_capture_pressure() {
        let source = b"#If First Then\nSub One()\nEnd Sub\n#ElseIf Second Then\n#Else\nSub Two()\nEnd Sub\n#End If\n";
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&language_for(GrammarFamily::Vba))
            .unwrap();
        let tree = parser.parse(source, None).unwrap();
        let registry = QueryPackRegistry::audited().unwrap();
        let extraction = registry
            .get(GrammarFamily::Vba)
            .unwrap()
            .extract(
                GrammarFamily::Vba,
                &tree,
                source,
                10_000,
                1,
                &Cancellation::new(),
            )
            .unwrap();
        for syntax in [
            "vba.compilation_if",
            "vba.compilation_elseif",
            "vba.compilation_else",
        ] {
            let headers: Vec<_> = extraction
                .candidates
                .iter()
                .filter(|candidate| candidate.syntax == syntax)
                .collect();
            assert!(!headers.is_empty(), "{syntax}");
            assert!(headers.iter().any(|header| header.required), "{syntax}");
        }
    }
}

#[cfg(test)]
mod file_root_bom_tests {
    use super::*;

    const SOURCE: &[u8] = b"\xef\xbb\xbf---\nconst {posts} = Astro.props;\n---\n{posts}\n";

    fn parsed(partial: bool) -> tree_sitter::Tree {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&language_for(GrammarFamily::Astro))
            .unwrap();
        if partial {
            parser
                .set_included_ranges(&[tree_sitter::Range {
                    start_byte: 3,
                    end_byte: SOURCE.len(),
                    start_point: tree_sitter::Point::new(0, 3),
                    end_point: tree_sitter::Point::new(4, 0),
                }])
                .unwrap();
        }
        let tree = parser.parse(SOURCE, None).unwrap();
        assert!(!tree.root_node().has_error());
        assert!(!tree.root_node().is_missing());
        assert_eq!(tree.root_node().start_byte(), 3);
        assert_eq!(tree.root_node().end_byte(), SOURCE.len());
        tree
    }

    #[test]
    fn complete_native_file_root_captures_include_only_the_known_bom_preamble() {
        let tree = parsed(false);
        let pack =
            QueryPack::compile(GrammarFamily::Astro, include_str!("../queries/astro.scm")).unwrap();
        let candidates = pack
            .extract_identity(
                GrammarFamily::Astro,
                &tree,
                SOURCE,
                16_384,
                &Cancellation::new(),
            )
            .unwrap();
        for role in [StructuralRole::Root, StructuralRole::Module] {
            let roots: Vec<_> = candidates
                .iter()
                .filter(|item| item.role == role && item.syntax == "astro.file")
                .collect();
            assert_eq!(roots.len(), 1);
            assert_eq!(roots[0].start..roots[0].end, 0..SOURCE.len());
            assert!(roots[0].required);
        }
        let frontmatter: Vec<_> = candidates
            .iter()
            .filter(|item| item.syntax == "astro.frontmatter")
            .collect();
        assert_eq!(frontmatter.len(), 1);
        assert!(
            frontmatter[0].start >= 3,
            "a child capture cannot absorb the file preamble"
        );
        assert_eq!(
            &SOURCE[frontmatter[0].start..frontmatter[0].end],
            b"\nconst {posts} = Astro.props;"
        );
    }

    #[test]
    fn partial_native_ranges_do_not_expand_file_root_captures_across_a_bom() {
        let tree = parsed(true);
        let pack =
            QueryPack::compile(GrammarFamily::Astro, include_str!("../queries/astro.scm")).unwrap();
        let cancel = Cancellation::new();
        let candidates = pack
            .extract_identity(GrammarFamily::Astro, &tree, SOURCE, 16_384, &cancel)
            .unwrap();
        for role in [StructuralRole::Root, StructuralRole::Module] {
            let roots: Vec<_> = candidates
                .iter()
                .filter(|item| item.role == role && item.syntax == "astro.file")
                .collect();
            assert_eq!(roots.len(), 1);
            assert_eq!(roots[0].start..roots[0].end, 3..SOURCE.len());
        }
        cancel.cancel(rootlight_cancel::CancellationReason::ClientRequest);
        assert!(matches!(
            pack.extract_identity(GrammarFamily::Astro, &tree, SOURCE, 16_384, &cancel),
            Err(AdapterError::Cancelled { .. })
        ));
    }
}
