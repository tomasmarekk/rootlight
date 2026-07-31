//! Reviewed structural query packs for Rootlight's audited grammars.
//!
//! Native queries and capture indices stay private; runtime extraction sees
//! only the closed, parser-independent role mapping defined here.

use std::{cmp::Ordering, collections::BinaryHeap, ops::ControlFlow};

use rootlight_adapter_sdk::{AdapterError, DiagnosticCode, SyntaxFactKind};
use rootlight_cancel::Cancellation;
use tree_sitter::{Query, QueryCursor, QueryCursorOptions, StreamingIterator};

use crate::{GrammarFamily, registry::language_for};

const QUERY_CURSOR_MATCH_LIMIT: u32 = 4096;
const HARD_MAX_QUERY_MATCHES: usize = 1_048_576;
const HARD_MAX_QUERY_CAPTURES: usize = 2_097_152;
const HARD_MAX_QUERY_FACTS: usize = 1_048_576;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
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
            Self::Definition | Self::Call | Self::ScopedCall | Self::Reference => {
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
            Self::Reference => 8,
            Self::Comment => 9,
            Self::StringLiteral => 10,
        }
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
}

impl QueryCandidate {
    pub(crate) fn retention_rank(self) -> (u8, usize, usize, StructuralRole, &'static str) {
        (
            self.role.retention_rank(),
            self.start,
            self.end,
            self.role,
            self.syntax,
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RetainedCandidate(QueryCandidate);

impl RetainedCandidate {
    fn rank(self) -> (u8, usize, usize, StructuralRole, &'static str) {
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

pub(crate) struct QueryPack {
    query: Query,
    roles_by_capture: Vec<StructuralRole>,
}

impl QueryPack {
    fn compile(family: GrammarFamily, source: &str) -> Result<Self, GrammarFamily> {
        let query = Query::new(&language_for(family), source).map_err(|_| family)?;
        let mut expected = EXPECTED_CAPTURES.to_vec();
        if family == GrammarFamily::Rust {
            expected.extend(RUST_SPECIAL_CAPTURES);
            expected.sort_unstable();
        }
        let mut observed = query.capture_names().to_vec();
        observed.sort_unstable();
        if observed != expected {
            return Err(family);
        }
        let roles_by_capture = query
            .capture_names()
            .iter()
            .map(|name| StructuralRole::from_capture_name(name).ok_or(family))
            .collect::<Result<Vec<_>, _>>()?;
        let pack = Self {
            query,
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
        let max_matches = max_nodes
            .checked_mul(8)
            .ok_or_else(|| query_failure("query-match-accounting"))?
            .min(HARD_MAX_QUERY_MATCHES);
        let max_captures = max_nodes
            .checked_mul(8)
            .ok_or_else(|| query_failure("query-capture-accounting"))?
            .min(HARD_MAX_QUERY_CAPTURES);
        let max_facts = max_facts.min(HARD_MAX_QUERY_FACTS);
        if max_matches == 0 || max_captures == 0 || max_facts == 0 {
            return Ok(QueryExtraction {
                candidates: Vec::new(),
                limit: Some(QueryLimit::Fact),
                fact_limit: max_facts,
            });
        }

        let mut cursor = QueryCursor::new();
        cursor.set_match_limit(QUERY_CURSOR_MATCH_LIMIT);
        let mut callback_cancelled = false;
        let mut progress = |_: &tree_sitter::QueryCursorState| {
            if cancellation.check().is_ok() {
                ControlFlow::Continue(())
            } else {
                callback_cancelled = true;
                ControlFlow::Break(())
            }
        };
        let options = QueryCursorOptions::new().progress_callback(&mut progress);
        let mut matches =
            cursor.matches_with_options(&self.query, tree.root_node(), source, options);
        let mut candidates = BinaryHeap::with_capacity(max_facts.min(4096));
        let mut match_count = 0usize;
        let mut capture_count = 0usize;
        let mut limit = None;
        let mut fact_limit_reached = false;

        'query: while let Some(query_match) = matches.next() {
            if match_count >= max_matches {
                limit = Some(QueryLimit::Match);
                break;
            }
            match_count = match_count
                .checked_add(1)
                .ok_or_else(|| query_failure("query-match-accounting"))?;
            for capture in query_match.captures {
                if capture_count >= max_captures {
                    limit = Some(QueryLimit::Capture);
                    break 'query;
                }
                capture_count = capture_count
                    .checked_add(1)
                    .ok_or_else(|| query_failure("query-capture-accounting"))?;
                let role = self
                    .role_for_capture(capture.index)
                    .ok_or_else(|| query_failure("query-capture-role"))?;
                // These roles identify reviewed Rust grammar fields rather than
                // the many concrete node kinds accepted by the `_type` rule.
                let syntax = match role {
                    StructuralRole::ScopeTrait => "rust.impl_trait",
                    StructuralRole::ScopeType => "rust.impl_type",
                    StructuralRole::TestAttribute => "rust.test_attribute",
                    StructuralRole::ScopedCall => "rust.scoped_call",
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
                    },
                    _ => canonical_syntax(family, capture.node.kind())
                        .ok_or_else(|| query_failure("query-node-kind"))?,
                };
                let candidate = RetainedCandidate(QueryCandidate {
                    start: capture.node.start_byte(),
                    end: capture.node.end_byte(),
                    role,
                    syntax,
                });
                if candidates.len() < max_facts {
                    candidates.push(candidate);
                    continue;
                }

                fact_limit_reached = true;
                // Continue the bounded query after capacity is reached so late
                // declarations can replace lower-value calls and references.
                let mut worst = candidates
                    .peek_mut()
                    .expect("a nonzero full fact heap has a worst candidate");
                if candidate < *worst {
                    *worst = candidate;
                }
            }
        }
        drop(matches);
        if callback_cancelled {
            cancellation.check()?;
        }
        cancellation.check()?;
        if cursor.did_exceed_match_limit() {
            limit = Some(QueryLimit::CursorMatch);
        } else if limit.is_none() && fact_limit_reached {
            limit = Some(QueryLimit::Fact);
        }
        Ok(QueryExtraction {
            candidates: candidates
                .into_iter()
                .map(|candidate| candidate.0)
                .collect(),
            limit,
            fact_limit: max_facts,
        })
    }
}

pub(crate) struct QueryPackRegistry {
    packs: Vec<(GrammarFamily, QueryPack)>,
}

fn query_failure(code: &'static str) -> AdapterError {
    AdapterError::ProviderFailed {
        code: DiagnosticCode::new(code).expect("built-in query failure code is valid"),
    }
}

fn canonical_syntax(family: GrammarFamily, native: &str) -> Option<&'static str> {
    match (family, native) {
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
        let mut packs = Vec::with_capacity(11);
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
        ] {
            packs.push((family, QueryPack::compile(family, source)?));
        }
        packs.sort_by_key(|(family, _)| *family);
        Ok(Self { packs })
    }

    pub(crate) fn get(&self, family: GrammarFamily) -> Option<&QueryPack> {
        self.packs
            .binary_search_by_key(&family, |(registered, _)| *registered)
            .ok()
            .and_then(|index| self.packs.get(index))
            .map(|(_, pack)| pack)
    }

    pub(crate) const fn len(&self) -> usize {
        self.packs.len()
    }

    pub(crate) fn pattern_count(&self) -> usize {
        self.packs
            .iter()
            .map(|(_, pack)| pack.query.pattern_count())
            .sum()
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
        ] {
            let pack = registry.get(family).expect("family has a query pack");
            let mut names = pack.query.capture_names().to_vec();
            names.sort_unstable();
            let mut expected = EXPECTED_CAPTURES.to_vec();
            if family == GrammarFamily::Rust {
                expected.extend(RUST_SPECIAL_CAPTURES);
                expected.sort_unstable();
            }
            assert_eq!(names, expected);
            assert_eq!(pack.roles_by_capture.len(), expected.len());
        }
    }
}
