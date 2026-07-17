//! Reviewed structural query packs for Rootlight's audited grammars.
//!
//! Native queries and capture indices stay private; runtime extraction sees
//! only the closed, parser-independent role mapping defined here.

use std::ops::ControlFlow;

use rootlight_adapter_sdk::{AdapterError, DiagnosticCode, SyntaxFactKind};
use rootlight_cancel::Cancellation;
use tree_sitter::{Query, QueryCursor, QueryCursorOptions, StreamingIterator};

use crate::{GrammarFamily, registry::language_for};

const QUERY_CURSOR_MATCH_LIMIT: u32 = 4096;
const HARD_MAX_QUERY_MATCHES: usize = 1_048_576;
const HARD_MAX_QUERY_CAPTURES: usize = 2_097_152;
const HARD_MAX_QUERY_FACTS: usize = 1_048_576;

const EXPECTED_CAPTURES: [&str; 11] = [
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum StructuralRole {
    Root,
    Module,
    Declaration,
    Signature,
    Import,
    Scope,
    Definition,
    Reference,
    Comment,
    Documentation,
    StringLiteral,
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
            "definition" => Some(Self::Definition),
            "reference" => Some(Self::Reference),
            "comment" => Some(Self::Comment),
            "documentation" => Some(Self::Documentation),
            "string" => Some(Self::StringLiteral),
            _ => None,
        }
    }

    pub(crate) const fn fact_kind(self) -> SyntaxFactKind {
        match self {
            Self::Root => SyntaxFactKind::Root,
            Self::Module => SyntaxFactKind::Module,
            Self::Declaration => SyntaxFactKind::Declaration,
            Self::Signature => SyntaxFactKind::Signature,
            Self::Import => SyntaxFactKind::Import,
            Self::Scope => SyntaxFactKind::Scope,
            Self::Definition | Self::Reference => SyntaxFactKind::Occurrence,
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
            Self::Definition => "definition",
            Self::Reference => "reference",
            Self::Comment => "comment",
            Self::Documentation => "documentation",
            Self::StringLiteral => "string",
        }
    }

    pub(crate) const fn container_rank(self) -> Option<u8> {
        match self {
            Self::Root => Some(0),
            Self::Module => Some(1),
            Self::Declaration => Some(2),
            Self::Scope => Some(3),
            _ => None,
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
        let mut observed = query.capture_names().to_vec();
        observed.sort_unstable();
        if observed != EXPECTED_CAPTURES {
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
        let mut candidates = Vec::with_capacity(max_facts.min(4096));
        let mut match_count = 0usize;
        let mut capture_count = 0usize;
        let mut limit = None;

        while let Some(query_match) = matches.next() {
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
                    break;
                }
                capture_count = capture_count
                    .checked_add(1)
                    .ok_or_else(|| query_failure("query-capture-accounting"))?;
                if candidates.len() >= max_facts {
                    limit = Some(QueryLimit::Fact);
                    break;
                }
                let role = self
                    .role_for_capture(capture.index)
                    .ok_or_else(|| query_failure("query-capture-role"))?;
                let syntax = canonical_syntax(family, capture.node.kind())
                    .ok_or_else(|| query_failure("query-node-kind"))?;
                candidates.push(QueryCandidate {
                    start: capture.node.start_byte(),
                    end: capture.node.end_byte(),
                    role,
                    syntax,
                });
            }
            if limit.is_some() {
                break;
            }
        }
        drop(matches);
        if callback_cancelled {
            cancellation.check()?;
        }
        cancellation.check()?;
        if cursor.did_exceed_match_limit() {
            limit = Some(QueryLimit::CursorMatch);
        }
        Ok(QueryExtraction {
            candidates,
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
        (GrammarFamily::Java, "variable_declarator") => Some("java.variable"),
        (GrammarFamily::Java, "import_declaration") => Some("java.import"),
        (GrammarFamily::Java, "formal_parameters") => Some("java.parameters"),
        (GrammarFamily::Java, "(") => Some("java.parameters"),
        (GrammarFamily::Java, "block") => Some("java.block"),
        (GrammarFamily::Java, "identifier") => Some("java.identifier"),
        (GrammarFamily::Java, "line_comment") => Some("java.line_comment"),
        (GrammarFamily::Java, "block_comment") => Some("java.block_comment"),
        (GrammarFamily::Java, "string_literal") => Some("java.string"),
        _ => None,
    }
}

impl QueryPackRegistry {
    pub(crate) fn audited() -> Result<Self, GrammarFamily> {
        let mut packs = Vec::with_capacity(4);
        for (family, source) in [
            (GrammarFamily::Rust, include_str!("../queries/rust.scm")),
            (GrammarFamily::Python, include_str!("../queries/python.scm")),
            (
                GrammarFamily::JavaScript,
                include_str!("../queries/javascript.scm"),
            ),
            (GrammarFamily::Java, include_str!("../queries/java.scm")),
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
        ] {
            let pack = registry.get(family).expect("family has a query pack");
            let mut names = pack.query.capture_names().to_vec();
            names.sort_unstable();
            assert_eq!(names, EXPECTED_CAPTURES);
            assert_eq!(pack.roles_by_capture.len(), EXPECTED_CAPTURES.len());
        }
    }
}
