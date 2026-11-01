//! Typed RDF-family statements over exact physical source.
//! Canonical envelopes retain decoded term roles without inventing software
//! declarations; native Dgraph values remain distinct from standard RDF terms.

use std::io::Write;

use rootlight_ids::{FactId, GenerationId, derive_fact};
use serde::{Deserialize, Serialize};

use crate::{
    ExtensionCriticality, ExtensionEnvelope, FactEvidence, FactRef, SourceRef, SourceSpan,
};

/// Namespace for validated source-backed RDF statements.
pub const RDF_STATEMENT_NAMESPACE: &str = "rootlight.rdf_statement";
/// Maximum canonical RDF statement payload, matching the default IR payload limit.
pub const MAX_RDF_STATEMENT_PAYLOAD_BYTES: usize = 1024 * 1024;
const XSD_STRING: &str = "http://www.w3.org/2001/XMLSchema#string";
const RDF_LANG_STRING: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#langString";

/// The native source format authenticated by a statement envelope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RdfStatementFormat {
    /// A three-term N-Triples statement encoded with the original v1 contract.
    NTriples,
    /// An N-Quads statement in the default graph or a written named graph.
    NQuads,
    /// Dgraph angle identifiers and default values, without namespace or facets.
    NQuadsDgraph,
}

impl RdfStatementFormat {
    fn is_ntriples(&self) -> bool {
        *self == Self::NTriples
    }
}

/// The category of a validated source term, including explicit native values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RdfTermKind {
    /// An absolute internationalized resource identifier.
    Iri,
    /// A label whose identity is scoped to its source document and generation.
    BlankNode,
    /// A lexical value with an effective datatype and optional language tag.
    Literal,
    /// A verbatim Dgraph identifier, without asserted absolute-IRI or UID identity.
    DgraphIdentifier,
    /// Native Dgraph default-value bytes, without an asserted RDF datatype.
    DgraphDefaultValue,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum TermValue {
    Iri {
        value: String,
    },
    BlankNode {
        label: String,
    },
    Literal {
        value: String,
        language: Option<String>,
        datatype: Option<String>,
    },
    DgraphIdentifier {
        value: String,
    },
    DgraphDefaultValue {
        value: Vec<u8>,
    },
}

/// One validated native term and its exact generation-bound physical spelling.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RdfTerm {
    source: SourceRef,
    value: TermValue,
}

impl RdfTerm {
    /// Returns the physical source identity and half-open term range.
    #[must_use]
    pub const fn source(&self) -> &SourceRef {
        &self.source
    }

    /// Returns the native term category, independently of its statement role.
    #[must_use]
    pub const fn kind(&self) -> RdfTermKind {
        match self.value {
            TermValue::Iri { .. } => RdfTermKind::Iri,
            TermValue::BlankNode { .. } => RdfTermKind::BlankNode,
            TermValue::Literal { .. } => RdfTermKind::Literal,
            TermValue::DgraphIdentifier { .. } => RdfTermKind::DgraphIdentifier,
            TermValue::DgraphDefaultValue { .. } => RdfTermKind::DgraphDefaultValue,
        }
    }

    /// Returns the decoded absolute IRI for an IRI term.
    #[must_use]
    pub fn iri(&self) -> Option<&str> {
        match &self.value {
            TermValue::Iri { value } => Some(value),
            _ => None,
        }
    }

    /// Returns a Dgraph identifier's verbatim interior, retaining written escapes.
    ///
    /// This does not resolve an identifier to a database UID or an absolute IRI.
    #[must_use]
    pub fn dgraph_identifier(&self) -> Option<&str> {
        match &self.value {
            TermValue::DgraphIdentifier { value } => Some(value),
            _ => None,
        }
    }

    /// Returns native default-value bytes, including byte escapes without coercion.
    #[must_use]
    pub fn dgraph_default_value(&self) -> Option<&[u8]> {
        match &self.value {
            TermValue::DgraphDefaultValue { value } => Some(value),
            _ => None,
        }
    }

    /// Returns a UTF-8 view only when all native default-value bytes form UTF-8.
    #[must_use]
    pub fn dgraph_default_text(&self) -> Option<&str> {
        std::str::from_utf8(self.dgraph_default_value()?).ok()
    }

    /// Returns the written blank-node label without the `_:` introducer.
    #[must_use]
    pub fn blank_node_label(&self) -> Option<&str> {
        match &self.value {
            TermValue::BlankNode { label } => Some(label),
            _ => None,
        }
    }

    /// Compares blank-node identity within the same immutable document and generation.
    #[must_use]
    pub fn same_blank_node(&self, other: &Self) -> bool {
        self.blank_node_label().is_some()
            && self.blank_node_label() == other.blank_node_label()
            && self.source.repository() == other.source.repository()
            && self.source.generation() == other.source.generation()
            && self.source.span().file() == other.source.span().file()
            && self.source.content_hash() == other.source.content_hash()
    }

    /// Returns the decoded lexical value of a literal.
    #[must_use]
    pub fn literal_value(&self) -> Option<&str> {
        match &self.value {
            TermValue::Literal { value, .. } => Some(value),
            _ => None,
        }
    }

    /// Returns a literal's canonical lowercase language tag, when written.
    #[must_use]
    pub fn language(&self) -> Option<&str> {
        match &self.value {
            TermValue::Literal { language, .. } => language.as_deref(),
            _ => None,
        }
    }

    /// Returns a literal's effective datatype, including implicit RDF datatypes.
    #[must_use]
    pub fn datatype(&self) -> Option<&str> {
        match &self.value {
            TermValue::Literal {
                language: Some(_), ..
            } => Some(RDF_LANG_STRING),
            TermValue::Literal { datatype, .. } => Some(datatype.as_deref().unwrap_or(XSD_STRING)),
            _ => None,
        }
    }
}

/// A physical RDF statement with typed term roles and optional named-graph evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RdfStatement {
    source: SourceRef,
    text: String,
    subject: RdfTerm,
    predicate: RdfTerm,
    object: RdfTerm,
    #[serde(skip_serializing_if = "RdfStatementFormat::is_ntriples")]
    format: RdfStatementFormat,
    #[serde(skip_serializing_if = "Option::is_none")]
    graph: Option<RdfTerm>,
}

/// Malformed, unsupported or noncanonical RDF statement evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("invalid rdf statement evidence")]
pub struct RdfStatementError;

/// Refusal to authenticate a statement in the bounded native Dgraph format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum DgraphStatementError {
    /// Source ownership, lexical syntax, term roles or statement layout is invalid.
    #[error("invalid dgraph statement evidence")]
    InvalidEvidence,
    /// Exact native letter/digit membership is unavailable for a Unicode-only ID.
    #[error("unsupported dgraph identifier category")]
    UnsupportedIdentifierCategory,
    /// A namespace, facet, blank label, language, typed value or function is unmodeled.
    #[error("unsupported dgraph statement construct")]
    UnsupportedConstruct,
}

impl RdfStatement {
    /// Decodes exact native N-Triples spans and their complete statement spelling.
    ///
    /// # Errors
    /// Rejects foreign, empty, overlapping or out-of-statement spans; invalid term
    /// roles, escapes and absolute IRIs; missing terminators and fourth graph terms.
    /// The caller must bind `text` to the immutable source identified by `source`.
    pub fn from_ntriples(
        source: SourceRef,
        text: &str,
        spans: [SourceSpan; 3],
    ) -> Result<Self, RdfStatementError> {
        Self::from_spans(RdfStatementFormat::NTriples, source, text, spans, None)
    }

    /// Decodes standard N-Quads roles over exact physical statement spans.
    ///
    /// An absent `graph` denotes the default graph without synthesizing a source
    /// term. A written graph must be an absolute IRI or a document-scoped blank node.
    /// The caller must bind `text` to the immutable source identified by `source`.
    ///
    /// # Errors
    /// Rejects invalid ownership, order, UTF-8 boundaries, term roles or spellings;
    /// relative IRIs, extra terms, multiline statements and missing terminators.
    pub fn from_nquads(
        source: SourceRef,
        text: &str,
        spans: [SourceSpan; 3],
        graph: Option<SourceSpan>,
    ) -> Result<Self, RdfStatementError> {
        Self::from_spans(RdfStatementFormat::NQuads, source, text, spans, graph)
    }

    /// Authenticates native Dgraph identifiers and untyped default-value bytes.
    ///
    /// Angle identifiers retain their interior spelling; they are neither absolute
    /// IRIs nor resolved database UIDs. No RDF graph or datatype is synthesized.
    /// The caller must bind `text` to the immutable source identified by `source`.
    ///
    /// # Errors
    /// Rejects invalid source spans, lexical syntax and roles. Namespace terms,
    /// facets, blank labels, functions, language predicates and typed/language
    /// values remain explicitly unsupported. Identifiers without an ASCII letter
    /// or digit report unavailable Unicode-category evidence rather than being
    /// classified as malformed when a non-ASCII category may be meaningful.
    pub fn from_nquads_dgraph(
        source: SourceRef,
        text: &str,
        spans: [SourceSpan; 3],
    ) -> Result<Self, DgraphStatementError> {
        if text.len() > MAX_RDF_STATEMENT_PAYLOAD_BYTES {
            return Err(DgraphStatementError::InvalidEvidence);
        }
        for (index, span) in spans.iter().enumerate() {
            let raw = source_slice(&source, text, span.start_byte(), span.end_byte())
                .map_err(|_| DgraphStatementError::InvalidEvidence)?;
            if raw.starts_with('<') {
                let identifier = dgraph_identifier(raw)?;
                if index == 1 && identifier.contains('@') {
                    return Err(DgraphStatementError::UnsupportedConstruct);
                }
            } else if raw.starts_with("_:")
                || raw.starts_with("uid(")
                || raw.starts_with("val(")
                || (index == 2
                    && raw.starts_with('"')
                    && dgraph_literal_tail(raw).is_some_and(|tail| !tail.is_empty()))
            {
                return Err(DgraphStatementError::UnsupportedConstruct);
            }
        }
        let tail = source_slice(&source, text, spans[2].end_byte(), source.span().end_byte())
            .map_err(|_| DgraphStatementError::InvalidEvidence)?
            .trim_start_matches([' ', '\t']);
        if tail.starts_with(['<', '(']) || tail.starts_with("_:") {
            return Err(DgraphStatementError::UnsupportedConstruct);
        }
        Self::from_spans(RdfStatementFormat::NQuadsDgraph, source, text, spans, None)
            .map_err(|_| DgraphStatementError::InvalidEvidence)
    }

    fn from_spans(
        format: RdfStatementFormat,
        source: SourceRef,
        text: &str,
        spans: [SourceSpan; 3],
        graph: Option<SourceSpan>,
    ) -> Result<Self, RdfStatementError> {
        if text.len() > MAX_RDF_STATEMENT_PAYLOAD_BYTES
            || u64::try_from(text.len()).ok()
                != source
                    .span()
                    .end_byte()
                    .checked_sub(source.span().start_byte())
            || spans[0].start_byte() != source.span().start_byte()
        {
            return Err(RdfStatementError);
        }
        let mut terms = Vec::with_capacity(if graph.is_some() { 4 } else { 3 });
        let mut previous = source.span().start_byte();
        for span in spans.into_iter().chain(graph) {
            if span.file() != source.span().file()
                || span.start_byte() < previous
                || span.start_byte() >= span.end_byte()
                || span.end_byte() > source.span().end_byte()
            {
                return Err(RdfStatementError);
            }
            let gap = source_slice(&source, text, previous, span.start_byte())?;
            if !gap.bytes().all(|byte| matches!(byte, b' ' | b'\t')) {
                return Err(RdfStatementError);
            }
            let raw = source_slice(&source, text, span.start_byte(), span.end_byte())?;
            if gap.is_empty()
                && terms
                    .last()
                    .is_some_and(|term: &RdfTerm| term.kind() == RdfTermKind::BlankNode)
                && raw.starts_with("_:")
            {
                return Err(RdfStatementError);
            }
            terms.push(RdfTerm {
                source: SourceRef::new(
                    source.repository(),
                    source.generation(),
                    span,
                    source.content_hash(),
                    None,
                ),
                value: if format == RdfStatementFormat::NQuadsDgraph {
                    parse_dgraph_term(raw)?
                } else {
                    parse_term(raw)?
                },
            });
            previous = span.end_byte();
        }
        let tail = source_slice(&source, text, previous, source.span().end_byte())?;
        if tail.trim_start_matches([' ', '\t']) != "." {
            return Err(RdfStatementError);
        }
        let mut terms = terms.into_iter();
        let subject = terms.next().ok_or(RdfStatementError)?;
        let predicate = terms.next().ok_or(RdfStatementError)?;
        let object = terms.next().ok_or(RdfStatementError)?;
        let graph = terms.next();
        let valid_roles = if format == RdfStatementFormat::NQuadsDgraph {
            subject.kind() == RdfTermKind::DgraphIdentifier
                && predicate.kind() == RdfTermKind::DgraphIdentifier
                && !predicate
                    .dgraph_identifier()
                    .is_some_and(|id| id.contains('@'))
                && matches!(
                    object.kind(),
                    RdfTermKind::DgraphIdentifier | RdfTermKind::DgraphDefaultValue
                )
                && graph.is_none()
        } else {
            subject.kind() != RdfTermKind::Literal
                && predicate.kind() == RdfTermKind::Iri
                && !graph
                    .as_ref()
                    .is_some_and(|term| term.kind() == RdfTermKind::Literal)
        };
        if !valid_roles {
            return Err(RdfStatementError);
        }
        Ok(Self {
            source,
            text: text.to_owned(),
            subject,
            predicate,
            object,
            format,
            graph,
        })
    }

    /// Returns the complete physical statement source, ending at its terminator.
    #[must_use]
    pub const fn source(&self) -> &SourceRef {
        &self.source
    }
    /// Returns the exact statement spelling authenticated by this payload.
    #[must_use]
    pub fn source_text(&self) -> &str {
        &self.text
    }
    /// Returns the resource in subject position.
    #[must_use]
    pub const fn subject(&self) -> &RdfTerm {
        &self.subject
    }
    /// Returns the predicate: a standard absolute IRI or native Dgraph identifier.
    #[must_use]
    pub const fn predicate(&self) -> &RdfTerm {
        &self.predicate
    }
    /// Returns the standard RDF term or native Dgraph value in object position.
    #[must_use]
    pub const fn object(&self) -> &RdfTerm {
        &self.object
    }

    /// Returns the native source format authenticated by this statement.
    #[must_use]
    pub const fn format(&self) -> RdfStatementFormat {
        self.format
    }

    /// Returns the written named graph, or `None` for an N-Quads default graph.
    ///
    /// N-Triples and the supported Dgraph default namespace also have no RDF graph;
    /// [`Self::format`] distinguishes these meanings.
    #[must_use]
    pub const fn graph(&self) -> Option<&RdfTerm> {
        self.graph.as_ref()
    }

    /// Checks exact immutable source bytes, separately from envelope integrity.
    ///
    /// # Errors
    /// Rejects any spelling differing from the source-backed statement payload.
    pub fn verify_source_text(&self, text: &str) -> Result<(), RdfStatementError> {
        if self.text == text {
            Ok(())
        } else {
            Err(RdfStatementError)
        }
    }

    pub(crate) fn rebind(self, generation: GenerationId) -> Result<Self, RdfStatementError> {
        let source = SourceRef::new(
            self.source.repository(),
            generation,
            self.source.span(),
            self.source.content_hash(),
            self.source.line_hint(),
        );
        Self::from_spans(
            self.format,
            source,
            &self.text,
            [
                self.subject.source.span(),
                self.predicate.source.span(),
                self.object.source.span(),
            ],
            self.graph.as_ref().map(|term| term.source.span()),
        )
    }
}

/// Encodes a typed statement using standard v1/v2 or native Dgraph v3 semantics.
///
/// # Errors
/// Rejects payloads exceeding the RDF/IR bound or allocation/encoding failures.
pub fn new_rdf_statement_envelope(
    provenance: FactId,
    statement: &RdfStatement,
) -> Result<ExtensionEnvelope, RdfStatementError> {
    let mut payload = BoundedPayload(Vec::new());
    serde_json::to_writer(&mut payload, statement).map_err(|_| RdfStatementError)?;
    let payload = String::from_utf8(payload.0).map_err(|_| RdfStatementError)?;
    let source = statement.source.clone();
    let derivation = vec![FactRef::File(source.span().file())];
    let (version, identity_domain) = match statement.format {
        RdfStatementFormat::NTriples => ("1", "rootlight.rdf_statement/v1"),
        RdfStatementFormat::NQuads => ("2", "rootlight.rdf_statement/v2"),
        RdfStatementFormat::NQuadsDgraph => ("3", "rootlight.rdf_statement/v3"),
    };
    let identity = serde_json::to_vec(&(
        RDF_STATEMENT_NAMESPACE,
        version,
        source.repository(),
        source.generation(),
        provenance,
        &source,
        &payload,
        &derivation,
    ))
    .map_err(|_| RdfStatementError)?;
    Ok(ExtensionEnvelope {
        id: derive_fact(identity_domain, &identity).id(),
        repository: source.repository(),
        generation: source.generation(),
        namespace: RDF_STATEMENT_NAMESPACE.to_owned(),
        version: version.to_owned(),
        criticality: ExtensionCriticality::Noncritical,
        payload,
        provenance,
        evidence: FactEvidence {
            source: Some(source),
            derivation,
        },
    })
}

/// Decodes and authenticates bounded RDF roles, source ownership and canonical IDs.
///
/// # Errors
/// Rejects malformed/oversized payloads, unknown versions, forged typed values,
/// altered source identity or noncanonical envelope encoding. Immutable source
/// bytes still require [`RdfStatement::verify_source_text`].
pub fn decode_rdf_statement_envelope(
    envelope: &ExtensionEnvelope,
) -> Result<RdfStatement, RdfStatementError> {
    if envelope.namespace != RDF_STATEMENT_NAMESPACE
        || envelope.payload.len() > MAX_RDF_STATEMENT_PAYLOAD_BYTES
    {
        return Err(RdfStatementError);
    }
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Term {
        source: SourceRef,
        value: TermValue,
    }
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Wire {
        source: SourceRef,
        text: String,
        subject: Term,
        predicate: Term,
        object: Term,
        format: Option<RdfStatementFormat>,
        graph: Option<Term>,
    }
    let wire: Wire = serde_json::from_str(&envelope.payload).map_err(|_| RdfStatementError)?;
    let format = match (envelope.version.as_str(), wire.format) {
        ("1", None) if wire.graph.is_none() => RdfStatementFormat::NTriples,
        ("2", Some(RdfStatementFormat::NQuads)) => RdfStatementFormat::NQuads,
        ("3", Some(RdfStatementFormat::NQuadsDgraph)) if wire.graph.is_none() => {
            RdfStatementFormat::NQuadsDgraph
        }
        _ => return Err(RdfStatementError),
    };
    let statement = RdfStatement::from_spans(
        format,
        wire.source,
        &wire.text,
        [
            wire.subject.source.span(),
            wire.predicate.source.span(),
            wire.object.source.span(),
        ],
        wire.graph.as_ref().map(|term| term.source.span()),
    )?;
    if statement.subject.source != wire.subject.source
        || statement.subject.value != wire.subject.value
        || statement.predicate.source != wire.predicate.source
        || statement.predicate.value != wire.predicate.value
        || statement.object.source != wire.object.source
        || statement.object.value != wire.object.value
        || statement
            .graph
            .as_ref()
            .map(|term| (&term.source, &term.value))
            != wire.graph.as_ref().map(|term| (&term.source, &term.value))
        || new_rdf_statement_envelope(envelope.provenance, &statement)? != *envelope
    {
        return Err(RdfStatementError);
    }
    Ok(statement)
}

struct BoundedPayload(Vec<u8>);
impl Write for BoundedPayload {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if self
            .0
            .len()
            .checked_add(bytes.len())
            .is_none_or(|size| size > MAX_RDF_STATEMENT_PAYLOAD_BYTES)
        {
            return Err(std::io::Error::other("rdf payload limit"));
        }
        self.0
            .try_reserve_exact(bytes.len())
            .map_err(|_| std::io::Error::other("rdf payload allocation"))?;
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn source_slice<'a>(
    source: &SourceRef,
    text: &'a str,
    start: u64,
    end: u64,
) -> Result<&'a str, RdfStatementError> {
    let start = start
        .checked_sub(source.span().start_byte())
        .and_then(|n| usize::try_from(n).ok())
        .ok_or(RdfStatementError)?;
    let end = end
        .checked_sub(source.span().start_byte())
        .and_then(|n| usize::try_from(n).ok())
        .ok_or(RdfStatementError)?;
    text.get(start..end).ok_or(RdfStatementError)
}

fn dgraph_identifier(raw: &str) -> Result<&str, DgraphStatementError> {
    let invalid = DgraphStatementError::InvalidEvidence;
    let body = raw
        .strip_prefix('<')
        .and_then(|value| value.strip_suffix('>'))
        .ok_or(invalid)?;
    let mut chars = body.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            let digits = match chars.next() {
                Some('u') => 4,
                Some('U') => 8,
                _ => return Err(invalid),
            };
            for _ in 0..digits {
                if !chars.next().is_some_and(|c| c.is_ascii_hexdigit()) {
                    return Err(invalid);
                }
            }
        } else if c <= ' ' || "<>\"{}|^`".contains(c) {
            return Err(invalid);
        }
    }
    // Native sanity checks inspect verbatim spelling. Escaped Unicode is not
    // decoded here, and uncertain Unicode-only category evidence stays refused.
    if !body.bytes().any(|byte| byte.is_ascii_alphanumeric()) {
        return Err(if body.is_ascii() {
            invalid
        } else {
            DgraphStatementError::UnsupportedIdentifierCategory
        });
    }
    Ok(body)
}

fn dgraph_literal_tail(raw: &str) -> Option<&str> {
    let mut escaped = false;
    for (index, c) in raw.strip_prefix('"')?.char_indices() {
        if c == '"' && !escaped {
            return raw.get(index + 2..);
        }
        escaped = c == '\\' && !escaped;
    }
    None
}

fn parse_dgraph_term(raw: &str) -> Result<TermValue, RdfStatementError> {
    if raw.starts_with('<') {
        return Ok(TermValue::DgraphIdentifier {
            value: dgraph_identifier(raw)
                .map_err(|_| RdfStatementError)?
                .to_owned(),
        });
    }
    let body = raw.strip_prefix('"').ok_or(RdfStatementError)?;
    let mut chars = body.char_indices();
    let mut value = Vec::with_capacity(body.len());
    loop {
        let (index, c) = chars.next().ok_or(RdfStatementError)?;
        match c {
            '"' if index + 1 == body.len() => break,
            '"' | '\r' | '\n' => return Err(RdfStatementError),
            '\\' => {
                let (_, kind) = chars.next().ok_or(RdfStatementError)?;
                let digits = match kind {
                    'x' => 2,
                    'u' => 4,
                    'U' => 8,
                    _ => 0,
                };
                if digits != 0 {
                    let mut decoded = 0u32;
                    for _ in 0..digits {
                        decoded = decoded
                            .checked_mul(16)
                            .and_then(|value| {
                                chars
                                    .next()
                                    .and_then(|(_, c)| c.to_digit(16))
                                    .and_then(|digit| value.checked_add(digit))
                            })
                            .ok_or(RdfStatementError)?;
                    }
                    if kind == 'x' {
                        value.push(u8::try_from(decoded).map_err(|_| RdfStatementError)?);
                    } else {
                        let c = char::from_u32(decoded).ok_or(RdfStatementError)?;
                        value.extend_from_slice(c.encode_utf8(&mut [0; 4]).as_bytes());
                    }
                } else {
                    value.push(match kind {
                        't' => b'\t',
                        'b' => 8,
                        'n' => b'\n',
                        'r' => b'\r',
                        'f' => 12,
                        'v' => 11,
                        '"' => b'"',
                        '\\' => b'\\',
                        _ => return Err(RdfStatementError),
                    });
                }
            }
            c => value.extend_from_slice(c.encode_utf8(&mut [0; 4]).as_bytes()),
        }
    }
    Ok(TermValue::DgraphDefaultValue { value })
}

fn parse_term(raw: &str) -> Result<TermValue, RdfStatementError> {
    if raw.starts_with('<') {
        return Ok(TermValue::Iri {
            value: parse_iri(raw)?,
        });
    }
    if let Some(label) = raw.strip_prefix("_:") {
        let mut chars = label.chars();
        if !chars.next().is_some_and(|c| pn_u(c) || c.is_ascii_digit())
            || !chars.all(|c| pn(c) || c == '.')
            || label.ends_with('.')
        {
            return Err(RdfStatementError);
        }
        return Ok(TermValue::BlankNode {
            label: label.to_owned(),
        });
    }
    let Some(body) = raw.strip_prefix('"') else {
        return Err(RdfStatementError);
    };
    let mut chars = body.char_indices();
    let mut value = String::new();
    let tail = loop {
        let (index, c) = chars.next().ok_or(RdfStatementError)?;
        match c {
            '"' => break body.get(index + 1..).ok_or(RdfStatementError)?,
            '\\' => value.push(escape(&mut chars, true)?),
            '\r' | '\n' => return Err(RdfStatementError),
            c => value.push(c),
        }
    };
    let (language, datatype) = if tail.is_empty() {
        (None, None)
    } else if let Some(tag) = tail.strip_prefix('@') {
        let mut parts = tag.split('-');
        if !parts
            .next()
            .is_some_and(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_alphabetic()))
            || !parts.all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_alphanumeric()))
        {
            return Err(RdfStatementError);
        }
        (Some(tag.to_ascii_lowercase()), None)
    } else if let Some(iri) = tail.strip_prefix("^^") {
        let iri = parse_iri(iri)?;
        if iri == RDF_LANG_STRING {
            return Err(RdfStatementError);
        }
        (None, Some(iri))
    } else {
        return Err(RdfStatementError);
    };
    Ok(TermValue::Literal {
        value,
        language,
        datatype,
    })
}

fn parse_iri(raw: &str) -> Result<String, RdfStatementError> {
    let body = raw
        .strip_prefix('<')
        .and_then(|s| s.strip_suffix('>'))
        .ok_or(RdfStatementError)?;
    let mut decoded = String::new();
    let mut chars = body.char_indices();
    while let Some((_, c)) = chars.next() {
        if c == '\\' {
            decoded.push(escape(&mut chars, false)?);
        } else if c <= ' ' || "<>\"{}|^`".contains(c) {
            return Err(RdfStatementError);
        } else {
            decoded.push(c);
        }
    }
    if !absolute_iri(&decoded) {
        return Err(RdfStatementError);
    }
    Ok(decoded)
}

fn escape(chars: &mut std::str::CharIndices<'_>, string: bool) -> Result<char, RdfStatementError> {
    let (_, kind) = chars.next().ok_or(RdfStatementError)?;
    let digits = match kind {
        'u' => 4,
        'U' => 8,
        _ => 0,
    };
    if digits != 0 {
        let mut value = 0u32;
        for _ in 0..digits {
            value = value
                .checked_mul(16)
                .and_then(|v| {
                    chars
                        .next()
                        .and_then(|(_, c)| c.to_digit(16))
                        .and_then(|d| v.checked_add(d))
                })
                .ok_or(RdfStatementError)?;
        }
        return char::from_u32(value).ok_or(RdfStatementError);
    }
    if !string {
        return Err(RdfStatementError);
    }
    match kind {
        't' => Ok('\t'),
        'b' => Ok('\u{8}'),
        'n' => Ok('\n'),
        'r' => Ok('\r'),
        'f' => Ok('\u{c}'),
        '"' => Ok('"'),
        '\'' => Ok('\''),
        '\\' => Ok('\\'),
        _ => Err(RdfStatementError),
    }
}

fn pn_u(c: char) -> bool {
    c.is_ascii_alphabetic()
        || matches!(c, '_' | ':' | '\u{c0}'..='\u{d6}' | '\u{d8}'..='\u{f6}' | '\u{f8}'..='\u{2ff}' | '\u{370}'..='\u{37d}' | '\u{37f}'..='\u{1fff}' | '\u{200c}'..='\u{200d}' | '\u{2070}'..='\u{218f}' | '\u{2c00}'..='\u{2fef}' | '\u{3001}'..='\u{d7ff}' | '\u{f900}'..='\u{fdcf}' | '\u{fdf0}'..='\u{fffd}' | '\u{10000}'..='\u{effff}')
}
fn pn(c: char) -> bool {
    pn_u(c)
        || c.is_ascii_digit()
        || matches!(c, '-' | '\u{b7}' | '\u{300}'..='\u{36f}' | '\u{203f}'..='\u{2040}')
}

// RFC3987 component rules keep percent escapes and private-use query characters
// distinct. Scheme-specific dereferencing and IRI normalization are not performed.
fn absolute_iri(value: &str) -> bool {
    let Some((scheme, rest)) = value.split_once(':') else {
        return false;
    };
    let mut chars = scheme.chars();
    if !chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        || !chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
    {
        return false;
    }
    let (rest, fragment) = rest
        .split_once('#')
        .map_or((rest, None), |(a, b)| (a, Some(b)));
    if fragment.is_some_and(|s| !iri_component(s, |c| ipchar(c) || matches!(c, '/' | '?'))) {
        return false;
    }
    let (path, query) = rest
        .split_once('?')
        .map_or((rest, None), |(a, b)| (a, Some(b)));
    if query
        .is_some_and(|s| !iri_component(s, |c| ipchar(c) || matches!(c, '/' | '?') || iprivate(c)))
    {
        return false;
    }
    let path = if let Some(authority_path) = path.strip_prefix("//") {
        let (authority, path) = authority_path
            .split_once('/')
            .map_or((authority_path, ""), |(a, b)| (a, b));
        if !iri_authority(authority) {
            return false;
        }
        path
    } else {
        path
    };
    iri_component(path, |c| ipchar(c) || c == '/')
}
fn iri_authority(authority: &str) -> bool {
    let host_port = if let Some((user, host)) = authority.rsplit_once('@') {
        if !iri_component(user, |c| iunreserved(c) || subdelim(c) || c == ':') {
            return false;
        }
        host
    } else {
        authority
    };
    if let Some(body) = host_port.strip_prefix('[') {
        let Some((host, tail)) = body.split_once(']') else {
            return false;
        };
        if !tail.is_empty()
            && !tail
                .strip_prefix(':')
                .is_some_and(|p| p.bytes().all(|c| c.is_ascii_digit()))
        {
            return false;
        }
        return host.parse::<std::net::Ipv6Addr>().is_ok()
            || host.strip_prefix(['v', 'V']).is_some_and(|s| {
                s.split_once('.').is_some_and(|(v, a)| {
                    !v.is_empty()
                        && v.bytes().all(|c| c.is_ascii_hexdigit())
                        && !a.is_empty()
                        && a.chars()
                            .all(|c| c.is_ascii() && (iunreserved(c) || subdelim(c) || c == ':'))
                })
            });
    }
    let host = if let Some((host, port)) = host_port.rsplit_once(':') {
        if !port.bytes().all(|c| c.is_ascii_digit()) {
            return false;
        }
        host
    } else {
        host_port
    };
    iri_component(host, |c| iunreserved(c) || subdelim(c))
}
fn iri_component(value: &str, allowed: impl Fn(char) -> bool) -> bool {
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        if c == '%' {
            if !chars.next().is_some_and(|c| c.is_ascii_hexdigit())
                || !chars.next().is_some_and(|c| c.is_ascii_hexdigit())
            {
                return false;
            }
        } else if !allowed(c) {
            return false;
        }
    }
    true
}
fn subdelim(c: char) -> bool {
    "!$&'()*+,;=".contains(c)
}
fn ipchar(c: char) -> bool {
    iunreserved(c) || subdelim(c) || matches!(c, ':' | '@')
}
fn iunreserved(c: char) -> bool {
    c.is_ascii_alphanumeric()
        || matches!(c, '-' | '.' | '_' | '~' | '\u{a0}'..='\u{d7ff}' | '\u{f900}'..='\u{fdcf}' | '\u{fdf0}'..='\u{ffef}')
        || (('\u{10000}'..='\u{dfffd}').contains(&c) && u32::from(c) & 0xffff <= 0xfffd)
        || ('\u{e1000}'..='\u{efffd}').contains(&c)
}
fn iprivate(c: char) -> bool {
    matches!(c, '\u{e000}'..='\u{f8ff}' | '\u{f0000}'..='\u{ffffd}' | '\u{100000}'..='\u{10fffd}')
}

#[cfg(test)]
mod tests {
    use super::*;
    use rootlight_ids::{FileId, RepositoryId, content_hash};

    fn source_spans<const N: usize>(text: &str, raw: [&str; N]) -> (SourceRef, [SourceSpan; N]) {
        let file = FileId::from_bytes([3; 20]);
        let repository = RepositoryId::from_bytes([1; 16]);
        let generation = GenerationId::from_bytes([2; 20]);
        let source = SourceRef::new(
            repository,
            generation,
            SourceSpan::new(file, 0, u64::try_from(text.len()).unwrap()).unwrap(),
            content_hash(text.as_bytes()),
            None,
        );
        let mut from = 0;
        let spans = raw.map(|part| {
            let start = from + text.get(from..).unwrap().find(part).unwrap();
            from = start + part.len();
            SourceSpan::new(
                file,
                u64::try_from(start).unwrap(),
                u64::try_from(from).unwrap(),
            )
            .unwrap()
        });
        (source, spans)
    }

    fn triple(text: &str, raw: [&str; 3]) -> Result<RdfStatement, RdfStatementError> {
        let (source, spans) = source_spans(text, raw);
        RdfStatement::from_ntriples(source, text, spans)
    }

    fn quad(
        text: &str,
        raw: [&str; 3],
        graph: Option<&str>,
    ) -> Result<RdfStatement, RdfStatementError> {
        if let Some(graph) = graph {
            let (source, [subject, predicate, object, graph]) =
                source_spans(text, [raw[0], raw[1], raw[2], graph]);
            RdfStatement::from_nquads(source, text, [subject, predicate, object], Some(graph))
        } else {
            let (source, spans) = source_spans(text, raw);
            RdfStatement::from_nquads(source, text, spans, None)
        }
    }

    #[test]
    fn rdf_ntriples_v1_preserves_original_payload_bytes_and_fact_identity() {
        #[derive(Serialize)]
        struct LegacyV1<'a> {
            source: &'a SourceRef,
            text: &'a str,
            subject: &'a RdfTerm,
            predicate: &'a RdfTerm,
            object: &'a RdfTerm,
        }
        let statement = triple("_:a<p:>\"λ\"@EN.", ["_:a", "<p:>", "\"λ\"@EN"]).unwrap();
        let payload = serde_json::to_string(&LegacyV1 {
            source: statement.source(),
            text: statement.source_text(),
            subject: statement.subject(),
            predicate: statement.predicate(),
            object: statement.object(),
        })
        .unwrap();
        let provenance = FactId::from_bytes([4; 20]);
        let derivation = vec![FactRef::File(statement.source.span().file())];
        let identity = serde_json::to_vec(&(
            RDF_STATEMENT_NAMESPACE,
            "1",
            statement.source.repository(),
            statement.source.generation(),
            provenance,
            &statement.source,
            &payload,
            &derivation,
        ))
        .unwrap();
        let envelope = new_rdf_statement_envelope(provenance, &statement).unwrap();
        assert_eq!(statement.format(), RdfStatementFormat::NTriples);
        assert!(statement.graph().is_none());
        assert_eq!(envelope.version, "1");
        assert_eq!(envelope.payload.as_bytes(), payload.as_bytes());
        assert_eq!(
            envelope.id,
            derive_fact("rootlight.rdf_statement/v1", &identity).id()
        );
        assert_eq!(decode_rdf_statement_envelope(&envelope).unwrap(), statement);
    }

    #[test]
    fn rdf_nquads_v2_round_trip_preserves_default_and_named_graph_roles() {
        for (text, raw, graph) in [
            (
                "<s:><p:>\"λ\\u0000\"@EN.",
                ["<s:>", "<p:>", "\"λ\\u0000\"@EN"],
                None,
            ),
            (
                "<s:><p:><o:><urn:g#anchor>.",
                ["<s:>", "<p:>", "<o:>"],
                Some("<urn:g#anchor>"),
            ),
            ("_:a <p:> _:a _:a.", ["_:a", "<p:>", "_:a"], Some("_:a")),
            (
                "<s:><p:>\"λ\"^^<urn:type><urn:g>.",
                ["<s:>", "<p:>", "\"λ\"^^<urn:type>"],
                Some("<urn:g>"),
            ),
        ] {
            let statement = quad(text, raw, graph).unwrap();
            assert_eq!(statement.format(), RdfStatementFormat::NQuads);
            assert_eq!(statement.graph().is_some(), graph.is_some());
            assert_eq!(statement.source_text(), text);
            if let Some(term) = statement.graph() {
                let span = term.source().span();
                assert_eq!(
                    source_slice(statement.source(), text, span.start_byte(), span.end_byte())
                        .unwrap(),
                    graph.unwrap()
                );
                assert!(span.start_byte() >= statement.object.source.span().end_byte());
                if term.kind() == RdfTermKind::BlankNode {
                    assert!(term.same_blank_node(statement.subject()));
                    assert!(term.same_blank_node(statement.object()));
                }
            }
            let envelope =
                new_rdf_statement_envelope(FactId::from_bytes([4; 20]), &statement).unwrap();
            assert_eq!(envelope.version, "2");
            assert_eq!(decode_rdf_statement_envelope(&envelope).unwrap(), statement);
            assert!(statement.verify_source_text(text).is_ok());
            if graph.is_none() {
                assert_eq!(statement.object().literal_value(), Some("λ\0"));
                assert_eq!(statement.object().language(), Some("en"));
                let triple = triple(text, raw).unwrap();
                assert_ne!(
                    envelope.id,
                    new_rdf_statement_envelope(envelope.provenance, &triple)
                        .unwrap()
                        .id
                );
            }
        }
    }

    #[test]
    fn rdf_nquads_refuses_invalid_graph_roles_source_ranges_and_statement_endings() {
        for (text, raw, graph) in [
            ("_:a<p:>_:a_:g.", ["_:a", "<p:>", "_:a"], Some("_:g")),
            (
                "<s:><p:><o:>\"g\".",
                ["<s:>", "<p:>", "<o:>"],
                Some("\"g\""),
            ),
            (
                "<s:><p:><o:><relative>.",
                ["<s:>", "<p:>", "<o:>"],
                Some("<relative>"),
            ),
            (
                "<s:><p:><o:><urn:g><extra:>.",
                ["<s:>", "<p:>", "<o:>"],
                Some("<urn:g>"),
            ),
            ("<s:><p:><o:><urn:g>.", ["<s:>", "<p:>", "<o:>"], None),
            (
                "<s:><p:><o:>\n<urn:g>.",
                ["<s:>", "<p:>", "<o:>"],
                Some("<urn:g>"),
            ),
            (
                "<s:><p:><o:><urn:g>",
                ["<s:>", "<p:>", "<o:>"],
                Some("<urn:g>"),
            ),
            (
                "<s:><p:><o:><urn:g>. #comment",
                ["<s:>", "<p:>", "<o:>"],
                Some("<urn:g>"),
            ),
            (
                "<s:><p:><o:><urn:g>.\n",
                ["<s:>", "<p:>", "<o:>"],
                Some("<urn:g>"),
            ),
            (
                "<s:><p:><o:><urn:\0g>.",
                ["<s:>", "<p:>", "<o:>"],
                Some("<urn:\0g>"),
            ),
            (
                "<alice><follows><bob>.",
                ["<alice>", "<follows>", "<bob>"],
                None,
            ),
        ] {
            assert!(quad(text, raw, graph).is_err(), "{text:?}");
        }
        let statement = quad(
            "<s:><p:><o:><urn:λ>.",
            ["<s:>", "<p:>", "<o:>"],
            Some("<urn:λ>"),
        )
        .unwrap();
        let graph = statement.graph().unwrap().source().span();
        let spans = [
            statement.subject.source.span(),
            statement.predicate.source.span(),
            statement.object.source.span(),
        ];
        for invalid in [
            SourceSpan::new(
                FileId::from_bytes([8; 20]),
                graph.start_byte(),
                graph.end_byte(),
            )
            .unwrap(),
            SourceSpan::new(graph.file(), graph.start_byte(), graph.start_byte()).unwrap(),
            statement.object.source.span(),
            SourceSpan::new(
                graph.file(),
                graph.start_byte(),
                statement.source.span().end_byte() + 1,
            )
            .unwrap(),
            SourceSpan::new(graph.file(), graph.start_byte(), graph.start_byte() + 6).unwrap(),
        ] {
            assert!(
                RdfStatement::from_nquads(
                    statement.source.clone(),
                    &statement.text,
                    spans,
                    Some(invalid)
                )
                .is_err()
            );
        }
    }

    #[test]
    fn rdf_nquads_decoder_authenticates_graph_sources_values_and_version() {
        let statement = quad("_:a<p:>_:a _:g.", ["_:a", "<p:>", "_:a"], Some("_:g")).unwrap();
        let original = new_rdf_statement_envelope(FactId::from_bytes([4; 20]), &statement).unwrap();
        let graph = statement.graph().unwrap();
        let raw = serde_json::to_string(graph).unwrap();
        for mutate in [
            |term: &mut RdfTerm| term.source.generation = GenerationId::from_bytes([9; 20]),
            |term: &mut RdfTerm| term.source.repository = RepositoryId::from_bytes([9; 16]),
            |term: &mut RdfTerm| term.source.content_hash = content_hash(b"foreign"),
            |term: &mut RdfTerm| {
                term.source.span = SourceSpan::new(FileId::from_bytes([9; 20]), 10, 13).unwrap()
            },
            |term: &mut RdfTerm| {
                term.value = TermValue::BlankNode {
                    label: "other".into(),
                }
            },
            |term: &mut RdfTerm| {
                term.value = TermValue::Literal {
                    value: "g".into(),
                    language: None,
                    datatype: None,
                }
            },
        ] {
            let mut forged = graph.clone();
            mutate(&mut forged);
            let mut altered = original.clone();
            altered.payload =
                altered
                    .payload
                    .replacen(&raw, &serde_json::to_string(&forged).unwrap(), 1);
            let identity = serde_json::to_vec(&(
                RDF_STATEMENT_NAMESPACE,
                "2",
                altered.repository,
                altered.generation,
                altered.provenance,
                &altered.evidence.source,
                &altered.payload,
                &altered.evidence.derivation,
            ))
            .unwrap();
            altered.id = derive_fact("rootlight.rdf_statement/v2", &identity).id();
            assert!(decode_rdf_statement_envelope(&altered).is_err());
        }
        for version in ["1", "3"] {
            let mut altered = original.clone();
            altered.version = version.into();
            assert!(decode_rdf_statement_envelope(&altered).is_err());
        }
        for format in ["n_triples", "unknown"] {
            let mut altered = original.clone();
            altered.payload = altered
                .payload
                .replace("\"n_quads\"", &format!("\"{format}\""));
            assert!(decode_rdf_statement_envelope(&altered).is_err());
        }
    }

    #[test]
    fn rdf_statement_round_trip_retains_typed_roles_and_unicode_literal() {
        let text = "_:a <https://example.test/p> \"\\u03bb\"@EN .";
        let statement =
            triple(text, ["_:a", "<https://example.test/p>", "\"\\u03bb\"@EN"]).unwrap();
        assert_eq!(statement.subject().kind(), RdfTermKind::BlankNode);
        assert_eq!(statement.object().literal_value(), Some("λ"));
        assert_eq!(statement.object().language(), Some("en"));
        assert_eq!(statement.object().datatype(), Some(RDF_LANG_STRING));
        let envelope = new_rdf_statement_envelope(FactId::from_bytes([4; 20]), &statement).unwrap();
        assert_eq!(decode_rdf_statement_envelope(&envelope).unwrap(), statement);
        assert!(statement.verify_source_text(text).is_ok());
        assert!(statement.verify_source_text("different").is_err());
    }

    #[test]
    fn rdf_statement_roles_and_statement_boundaries_refuse_non_triples() {
        for (text, raw) in [
            ("\"s\" <p:> <o:> .", ["\"s\"", "<p:>", "<o:>"]),
            ("<s:> _:p <o:> .", ["<s:>", "_:p", "<o:>"]),
            ("<s:> \"p\" <o:> .", ["<s:>", "\"p\"", "<o:>"]),
            ("<s:> <p:> <o:> <g:> .", ["<s:>", "<p:>", "<o:>"]),
            ("<s:> <p:> <o:> _:g .", ["<s:>", "<p:>", "<o:>"]),
            ("<s:>\n<p:> <o:> .", ["<s:>", "<p:>", "<o:>"]),
            ("<s:> # comment\n<p:> <o:> .", ["<s:>", "<p:>", "<o:>"]),
            ("<s:> <p:> <o:>", ["<s:>", "<p:>", "<o:>"]),
            ("<s:> <p:> <o:> ; .", ["<s:>", "<p:>", "<o:>"]),
        ] {
            assert!(triple(text, raw).is_err(), "{text:?}");
        }
    }

    #[test]
    fn rdf_statement_rejects_foreign_overlapping_and_non_utf8_boundary_spans() {
        let original = triple("<s:><p:>\"λ\".", ["<s:>", "<p:>", "\"λ\""]).unwrap();
        let spans = [
            original.subject.source.span(),
            original.predicate.source.span(),
            original.object.source.span(),
        ];
        for invalid in [
            SourceSpan::new(FileId::from_bytes([8; 20]), 0, 4).unwrap(),
            SourceSpan::new(spans[0].file(), 0, 0).unwrap(),
            SourceSpan::new(spans[0].file(), 0, 40).unwrap(),
        ] {
            assert!(
                RdfStatement::from_ntriples(
                    original.source.clone(),
                    &original.text,
                    [invalid, spans[1], spans[2]]
                )
                .is_err()
            );
        }
        assert!(
            RdfStatement::from_ntriples(
                original.source.clone(),
                &original.text,
                [spans[0], spans[0], spans[2]]
            )
            .is_err()
        );
        assert!(
            RdfStatement::from_ntriples(
                original.source.clone(),
                &original.text,
                [
                    spans[0],
                    spans[1],
                    SourceSpan::new(spans[0].file(), 9, 10).unwrap()
                ]
            )
            .is_err()
        );
        let mut shortened = original.source.clone();
        shortened.span = SourceSpan::new(spans[0].file(), 0, 1).unwrap();
        assert!(RdfStatement::from_ntriples(shortened, &original.text, spans).is_err());
    }

    #[test]
    fn rdf_term_escapes_datatypes_and_document_scoped_blank_nodes_are_typed() {
        let statement = triple("_:same <p:> _:same .", ["_:same", "<p:>", "_:same"]).unwrap();
        assert!(statement.subject.same_blank_node(&statement.object));
        let rebound = statement
            .clone()
            .rebind(GenerationId::from_bytes([9; 20]))
            .unwrap();
        assert!(!statement.subject.same_blank_node(&rebound.object));
        let mut other = statement.object.clone();
        other.source.content_hash = content_hash(b"different document");
        assert!(!statement.subject.same_blank_node(&other));
        let statement = triple(
            "<s:><p:>\"\\t\\b\\n\\r\\f\\\"\\'\\\\\\U0001F642\\u0000\".",
            [
                "<s:>",
                "<p:>",
                "\"\\t\\b\\n\\r\\f\\\"\\'\\\\\\U0001F642\\u0000\"",
            ],
        )
        .unwrap();
        assert_eq!(
            statement.object.literal_value(),
            Some("\t\u{8}\n\r\u{c}\"'\\🙂\0")
        );
        assert_eq!(statement.object.datatype(), Some(XSD_STRING));
        let statement = triple(
            "<s:><p:>\"x\"^^<https://example.test/type>.",
            ["<s:>", "<p:>", "\"x\"^^<https://example.test/type>"],
        )
        .unwrap();
        assert_eq!(
            statement.object.datatype(),
            Some("https://example.test/type")
        );
        let nul = "<s:><p:>\"\0\".";
        assert_eq!(
            triple(nul, ["<s:>", "<p:>", "\"\0\""])
                .unwrap()
                .object
                .literal_value(),
            Some("\0")
        );
        for raw in [
            "_:a.",
            "_:-a",
            "_:a!",
            "\"x\"@en-",
            "\"x\"@123",
            "\"x\"@en^^<t:>",
            "\"x\"^^<http://www.w3.org/1999/02/22-rdf-syntax-ns#langString>",
            "\"\\uD800\"",
            "\"\\U00110000\"",
            "\"\\x20\"",
            "\"x\n\"",
        ] {
            assert!(parse_term(raw).is_err(), "{raw:?}");
        }
        assert!(parse_term("_:0:a.b").is_ok());
    }

    #[test]
    fn rdf_absolute_iris_validate_components_and_unicode_escape_values() {
        for (raw, value) in [
            ("<urn:x>", "urn:x"),
            (
                "<https://例.example/λ?x=\u{e000}#x>",
                "https://例.example/λ?x=\u{e000}#x",
            ),
            (
                "<http\\u003A//example.test/\\U0001F642>",
                "http://example.test/🙂",
            ),
            (
                "<http://user:pass@[2001:db8::1]:80/p%20q>",
                "http://user:pass@[2001:db8::1]:80/p%20q",
            ),
            ("<scheme://[v1.a:b]/>", "scheme://[v1.a:b]/"),
            ("<file:///path>", "file:///path"),
        ] {
            assert_eq!(parse_iri(raw).unwrap(), value);
        }
        for raw in [
            "<relative>",
            "<1bad:abc>",
            "<http://host/%zz>",
            "<http://host/%a>",
            "<http://[::1>",
            "<http://[not-ip]>",
            "<http://host:no-port>",
            "<http://u@v@host/>",
            "<http://host/[path]>",
            "<http://host/#a#b>",
            "<http://host/\u{e000}>",
            "<http://host/#\u{e000}>",
            "<http://host/\\u0000>",
            "<http://host/\\uD800>",
            "<http://host/\\U00110000>",
            "<http://host/\\u007F>",
            "<http://host/\\uFDD0>",
            "<http://host/\\U0001FFFF>",
            "<http://host/\\U000E0000>",
        ] {
            assert!(parse_iri(raw).is_err(), "{raw:?}");
        }
    }

    #[test]
    fn rdf_statement_decoder_authenticates_typed_values_sources_and_canonical_payload() {
        let statement = triple("<s:><p:>\"x\"@EN.", ["<s:>", "<p:>", "\"x\"@EN"]).unwrap();
        let original = new_rdf_statement_envelope(FactId::from_bytes([4; 20]), &statement).unwrap();
        for mutate in [
            |e: &mut ExtensionEnvelope| e.version = "2".into(),
            |e: &mut ExtensionEnvelope| e.namespace = "foreign".into(),
            |e: &mut ExtensionEnvelope| e.criticality = ExtensionCriticality::Critical,
            |e: &mut ExtensionEnvelope| e.payload.push(' '),
            |e: &mut ExtensionEnvelope| e.payload = " ".repeat(MAX_RDF_STATEMENT_PAYLOAD_BYTES + 1),
            |e: &mut ExtensionEnvelope| e.payload = "{".into(),
            |e: &mut ExtensionEnvelope| e.evidence.source = None,
            |e: &mut ExtensionEnvelope| e.evidence.derivation.clear(),
            |e: &mut ExtensionEnvelope| e.repository = RepositoryId::from_bytes([9; 16]),
            |e: &mut ExtensionEnvelope| e.generation = GenerationId::from_bytes([9; 20]),
            |e: &mut ExtensionEnvelope| e.id = FactId::from_bytes([9; 20]),
        ] {
            let mut altered = original.clone();
            mutate(&mut altered);
            assert!(decode_rdf_statement_envelope(&altered).is_err());
        }
        for path in ["hash", "owner", "role", "literal", "graph"] {
            let mut value: serde_json::Value = serde_json::from_str(&original.payload).unwrap();
            match path {
                "hash" => {
                    value["object"]["source"]["content_hash"] =
                        serde_json::to_value(content_hash(b"foreign")).unwrap()
                }
                "owner" => {
                    value["object"]["source"]["generation"] =
                        serde_json::to_value(GenerationId::from_bytes([9; 20])).unwrap()
                }
                "role" => {
                    value["predicate"]["value"] =
                        serde_json::json!({"kind":"blank_node","label":"p"})
                }
                "literal" => value["object"]["value"]["datatype"] = serde_json::json!(XSD_STRING),
                "graph" => value["graph"] = serde_json::json!("<g:>"),
                _ => unreachable!(),
            }
            let mut altered = original.clone();
            altered.payload = serde_json::to_string(&value).unwrap();
            assert!(decode_rdf_statement_envelope(&altered).is_err(), "{path}");
        }
    }

    #[test]
    fn rdf_statement_payload_limit_refuses_without_truncation() {
        let value = "x".repeat(MAX_RDF_STATEMENT_PAYLOAD_BYTES / 2);
        let literal = format!("\"{value}\"");
        let text = format!("<s:><p:>{literal}.");
        let statement = triple(&text, ["<s:>", "<p:>", &literal]).unwrap();
        assert!(new_rdf_statement_envelope(FactId::from_bytes([4; 20]), &statement).is_err());
        let mut writer = BoundedPayload(Vec::new());
        assert!(
            writer
                .write_all(&vec![b'x'; MAX_RDF_STATEMENT_PAYLOAD_BYTES])
                .is_ok()
        );
        assert!(writer.write_all(b"x").is_err());
        assert_eq!(writer.0.len(), MAX_RDF_STATEMENT_PAYLOAD_BYTES);
        assert!(writer.0.capacity() <= MAX_RDF_STATEMENT_PAYLOAD_BYTES);
    }

    fn dgraph(text: &str, raw: [&str; 3]) -> Result<RdfStatement, DgraphStatementError> {
        let (source, spans) = source_spans(text, raw);
        RdfStatement::from_nquads_dgraph(source, text, spans)
    }

    #[test]
    fn dgraph_original_statements_preserve_native_identifier_edges_and_default_values() {
        // Apache-2.0 fixture from dgraph-io/dgraph, revision
        // c587cba75f60445125459b46b4d81757a73384aa, dgraph/cmd/alpha/testdata.nq.
        // Upstream license: https://github.com/dgraph-io/dgraph/blob/c587cba75f60445125459b46b4d81757a73384aa/LICENSE.txt
        const ORIGINAL: &str = r#"<alice> <follows> <bob> .
<bob> <follows> <fred> .
<bob> <status> "I'm your Uncle Bob!" .
<charlie> <follows> <bob> .
<charlie> <follows> <dani> .
<dani> <follows> <bob> .
<dani> <follows> <greg> .
<dani> <status> "Smart Dani" .
<emily> <follows> <fred> .
<fred> <follows> <greg> .
<greg> <status> "Mr. Cool greg" .
"#;
        let expected = [
            ("alice", "follows", "<bob>", Some("bob")),
            ("bob", "follows", "<fred>", Some("fred")),
            ("bob", "status", "\"I'm your Uncle Bob!\"", None),
            ("charlie", "follows", "<bob>", Some("bob")),
            ("charlie", "follows", "<dani>", Some("dani")),
            ("dani", "follows", "<bob>", Some("bob")),
            ("dani", "follows", "<greg>", Some("greg")),
            ("dani", "status", "\"Smart Dani\"", None),
            ("emily", "follows", "<fred>", Some("fred")),
            ("fred", "follows", "<greg>", Some("greg")),
            ("greg", "status", "\"Mr. Cool greg\"", None),
        ];
        assert_eq!(ORIGINAL.len(), 316);
        assert_eq!(ORIGINAL.lines().count(), expected.len());
        let mut offset = 0;
        let mut edges = 0;
        let mut values = 0;
        for (line, (subject, predicate, raw_object, object)) in
            ORIGINAL.split_inclusive('\n').zip(expected)
        {
            let text = line.strip_suffix('\n').unwrap();
            let raw_subject = format!("<{subject}>");
            let raw_predicate = format!("<{predicate}>");
            let (source, spans) = source_spans(text, [&raw_subject, &raw_predicate, raw_object]);
            let spans = spans.map(|span| {
                SourceSpan::new(
                    span.file(),
                    span.start_byte() + offset,
                    span.end_byte() + offset,
                )
                .unwrap()
            });
            let source = SourceRef::new(
                source.repository(),
                source.generation(),
                SourceSpan::new(
                    source.span().file(),
                    offset,
                    offset + u64::try_from(text.len()).unwrap(),
                )
                .unwrap(),
                content_hash(ORIGINAL.as_bytes()),
                None,
            );
            let statement = RdfStatement::from_nquads_dgraph(source, text, spans).unwrap();
            assert_eq!(statement.format(), RdfStatementFormat::NQuadsDgraph);
            assert!(statement.graph().is_none());
            assert_eq!(statement.subject().dgraph_identifier(), Some(subject));
            assert_eq!(statement.predicate().dgraph_identifier(), Some(predicate));
            for term in [
                statement.subject(),
                statement.predicate(),
                statement.object(),
            ] {
                let span = term.source().span();
                assert_eq!(
                    term.source().content_hash(),
                    content_hash(ORIGINAL.as_bytes())
                );
                assert!(span.start_byte() >= offset);
                assert_eq!(
                    ORIGINAL
                        .get(
                            usize::try_from(span.start_byte()).unwrap()
                                ..usize::try_from(span.end_byte()).unwrap()
                        )
                        .unwrap(),
                    source_slice(statement.source(), text, span.start_byte(), span.end_byte())
                        .unwrap()
                );
                assert!(term.iri().is_none());
                assert!(term.datatype().is_none());
                assert!(term.blank_node_label().is_none());
            }
            if let Some(object) = object {
                edges += 1;
                assert_eq!(statement.object().dgraph_identifier(), Some(object));
            } else {
                values += 1;
                let value = raw_object
                    .strip_prefix('"')
                    .unwrap()
                    .strip_suffix('"')
                    .unwrap();
                assert_eq!(
                    statement.object().dgraph_default_value(),
                    Some(value.as_bytes())
                );
                assert_eq!(statement.object().dgraph_default_text(), Some(value));
                assert!(statement.object().literal_value().is_none());
            }
            let envelope =
                new_rdf_statement_envelope(FactId::from_bytes([4; 20]), &statement).unwrap();
            assert_eq!(envelope.version, "3");
            assert_eq!(decode_rdf_statement_envelope(&envelope).unwrap(), statement);
            offset += u64::try_from(line.len()).unwrap();
        }
        assert_eq!((edges, values), (8, 3));
        assert_eq!(offset, u64::try_from(ORIGINAL.len()).unwrap());
    }

    #[test]
    fn dgraph_identifiers_retain_angle_spelling_without_iri_or_uid_resolution() {
        for identifier in [
            "alice",
            "0x01",
            "http://example.org/alice",
            "_:label",
            "λ0",
            r"a\u0020b",
            r"a\UFFFFFFFF",
        ] {
            let raw = format!("<{identifier}>");
            let text = format!("{raw}<follows>{raw}.");
            let statement = dgraph(&text, [&raw, "<follows>", &raw]).unwrap();
            assert_eq!(statement.subject().dgraph_identifier(), Some(identifier));
            assert_eq!(statement.object().dgraph_identifier(), Some(identifier));
            assert_eq!(statement.subject().kind(), RdfTermKind::DgraphIdentifier);
            assert!(statement.subject().iri().is_none());
            assert!(!statement.subject().same_blank_node(statement.object()));
        }
        for raw in [
            "<>",
            "<:>",
            "<a b>",
            "<a\0b>",
            "<a<b>",
            r"<a\u00XX>",
            r"<a\n>",
        ] {
            let text = format!("{raw}<follows><bob>.");
            assert_eq!(
                dgraph(&text, [raw, "<follows>", "<bob>"]),
                Err(DgraphStatementError::InvalidEvidence),
                "{raw:?}"
            );
        }
        for raw in ["<λ>", "<一>", "<\u{0301}>"] {
            let text = format!("{raw}<follows><bob>.");
            assert_eq!(
                dgraph(&text, [raw, "<follows>", "<bob>"]),
                Err(DgraphStatementError::UnsupportedIdentifierCategory)
            );
        }
        let text = "<alice><follows><bob>.";
        let (source, spans) = source_spans(text, ["<alice>", "<follows>", "<bob>"]);
        assert!(RdfStatement::from_ntriples(source.clone(), text, spans).is_err());
        assert!(RdfStatement::from_nquads(source, text, spans, None).is_err());
    }

    #[test]
    fn dgraph_default_values_preserve_native_byte_and_unicode_escapes() {
        for (literal, bytes, text) in [
            (r#""""#, b"".as_slice(), Some("")),
            (
                r#""\x02\v\t\b\n\r\f\"\\""#,
                b"\x02\x0b\t\x08\n\r\x0c\"\\",
                Some("\x02\u{b}\t\u{8}\n\r\u{c}\"\\"),
            ),
            (r#""\xFF""#, b"\xff", None),
            (r#""\xC3\xA9""#, "é".as_bytes(), Some("é")),
            (r#""\u03BB\U0001F600""#, "λ😀".as_bytes(), Some("λ😀")),
            ("\"raw\0value\t\"", b"raw\0value\t", Some("raw\0value\t")),
        ] {
            let source = format!("<alice><status>{literal}.");
            let statement = dgraph(&source, ["<alice>", "<status>", literal]).unwrap();
            assert_eq!(statement.object().kind(), RdfTermKind::DgraphDefaultValue);
            assert_eq!(statement.object().dgraph_default_value(), Some(bytes));
            assert_eq!(statement.object().dgraph_default_text(), text);
            assert!(statement.object().datatype().is_none());
            assert!(statement.object().language().is_none());
            assert!(statement.object().literal_value().is_none());
        }
        for literal in [
            r#""\a""#,
            r#""\'""#,
            r#""\01""#,
            r#""\xG0""#,
            r#""\uD800""#,
            r#""\U00110000""#,
            "\"unterminated",
            "\"raw\nline\"",
        ] {
            let source = format!("<alice><status>{literal}.");
            assert_eq!(
                dgraph(&source, ["<alice>", "<status>", literal]),
                Err(DgraphStatementError::InvalidEvidence),
                "{literal:?}"
            );
        }
    }

    #[test]
    fn dgraph_unmodeled_constructs_stay_explicitly_unsupported() {
        for (text, raw) in [
            ("_:alice<follows><bob>.", ["_:alice", "<follows>", "<bob>"]),
            ("<alice><follows>_:bob.", ["<alice>", "<follows>", "_:bob"]),
            (
                "<alice><status@en>\"value\".",
                ["<alice>", "<status@en>", "\"value\""],
            ),
            (
                "<alice><status>\"value\"@en.",
                ["<alice>", "<status>", "\"value\"@en"],
            ),
            (
                "<alice><status>\"value\"^^<xs:string>.",
                ["<alice>", "<status>", "\"value\"^^<xs:string>"],
            ),
            (
                "<alice><follows><bob><0x10>.",
                ["<alice>", "<follows>", "<bob>"],
            ),
            (
                "<alice><follows><bob>_:namespace.",
                ["<alice>", "<follows>", "<bob>"],
            ),
            (
                "<alice><follows><bob>(friend=true).",
                ["<alice>", "<follows>", "<bob>"],
            ),
            ("uid(a)<follows><bob>.", ["uid(a)", "<follows>", "<bob>"]),
            (
                "<alice><follows>val(a).",
                ["<alice>", "<follows>", "val(a)"],
            ),
        ] {
            assert_eq!(
                dgraph(text, raw),
                Err(DgraphStatementError::UnsupportedConstruct),
                "{text}"
            );
        }
        for (text, raw) in [
            ("0x01<follows><bob>.", ["0x01", "<follows>", "<bob>"]),
            ("<alice><follows>0x01.", ["<alice>", "<follows>", "0x01"]),
            (
                "\"alice\"<follows><bob>.",
                ["\"alice\"", "<follows>", "<bob>"],
            ),
            (
                "<alice>\"follows\"<bob>.",
                ["<alice>", "\"follows\"", "<bob>"],
            ),
        ] {
            assert_eq!(
                dgraph(text, raw),
                Err(DgraphStatementError::InvalidEvidence),
                "{text}"
            );
        }
    }

    #[test]
    fn dgraph_v3_decoder_authenticates_native_values_sources_and_format() {
        let statement = dgraph(
            "<alice><status>\"value\".",
            ["<alice>", "<status>", "\"value\""],
        )
        .unwrap();
        let original = new_rdf_statement_envelope(FactId::from_bytes([4; 20]), &statement).unwrap();
        assert_eq!(decode_rdf_statement_envelope(&original).unwrap(), statement);
        for field in ["value", "format", "graph", "owner", "source"] {
            let mut payload: serde_json::Value = serde_json::from_str(&original.payload).unwrap();
            match field {
                "value" => payload["object"]["value"]["value"] = serde_json::json!([0, 1, 2]),
                "format" => payload["format"] = serde_json::json!("n_quads"),
                "graph" => payload["graph"] = payload["subject"].clone(),
                "owner" => {
                    payload["object"]["source"]["generation"] =
                        serde_json::to_value(GenerationId::from_bytes([9; 20])).unwrap()
                }
                "source" => payload["subject"]["source"]["span"]["end_byte"] = serde_json::json!(3),
                _ => unreachable!(),
            }
            let mut altered = original.clone();
            altered.payload = serde_json::to_string(&payload).unwrap();
            let identity = serde_json::to_vec(&(
                RDF_STATEMENT_NAMESPACE,
                "3",
                altered.repository,
                altered.generation,
                altered.provenance,
                &altered.evidence.source,
                &altered.payload,
                &altered.evidence.derivation,
            ))
            .unwrap();
            altered.id = derive_fact("rootlight.rdf_statement/v3", &identity).id();
            assert!(decode_rdf_statement_envelope(&altered).is_err(), "{field}");
        }
        for version in ["1", "2", "4"] {
            let mut altered = original.clone();
            altered.version = version.to_owned();
            assert!(decode_rdf_statement_envelope(&altered).is_err());
        }
        let standard = triple(
            "<urn:alice><urn:status>\"value\".",
            ["<urn:alice>", "<urn:status>", "\"value\""],
        )
        .unwrap();
        let native = dgraph(
            standard.source_text(),
            ["<urn:alice>", "<urn:status>", "\"value\""],
        )
        .unwrap();
        let native_envelope = new_rdf_statement_envelope(original.provenance, &native).unwrap();
        let standard_envelope = new_rdf_statement_envelope(original.provenance, &standard).unwrap();
        assert_ne!(native_envelope.id, standard_envelope.id);
        assert_ne!(native_envelope.payload, standard_envelope.payload);
    }

    #[test]
    fn dgraph_addition_preserves_legacy_nquads_v2_payload_bytes_and_ids() {
        #[derive(Serialize)]
        #[serde(tag = "kind", rename_all = "snake_case")]
        enum LegacyValue<'a> {
            Iri {
                value: &'a str,
            },
            BlankNode {
                label: &'a str,
            },
            Literal {
                value: &'a str,
                language: Option<&'a str>,
                datatype: Option<&'a str>,
            },
        }
        #[derive(Serialize)]
        struct LegacyTerm<'a> {
            source: &'a SourceRef,
            value: LegacyValue<'a>,
        }
        #[derive(Serialize)]
        struct LegacyV2<'a> {
            source: &'a SourceRef,
            text: &'a str,
            subject: LegacyTerm<'a>,
            predicate: LegacyTerm<'a>,
            object: LegacyTerm<'a>,
            format: &'static str,
            #[serde(skip_serializing_if = "Option::is_none")]
            graph: Option<LegacyTerm<'a>>,
        }
        for graph in [None, Some("<urn:graph>"), Some("_:graph")] {
            let text = format!("<urn:alice><urn:status>\"λ\"@EN{}.", graph.unwrap_or(""));
            let statement =
                quad(&text, ["<urn:alice>", "<urn:status>", "\"λ\"@EN"], graph).unwrap();
            let payload = serde_json::to_string(&LegacyV2 {
                source: statement.source(),
                text: &text,
                subject: LegacyTerm {
                    source: statement.subject().source(),
                    value: LegacyValue::Iri { value: "urn:alice" },
                },
                predicate: LegacyTerm {
                    source: statement.predicate().source(),
                    value: LegacyValue::Iri {
                        value: "urn:status",
                    },
                },
                object: LegacyTerm {
                    source: statement.object().source(),
                    value: LegacyValue::Literal {
                        value: "λ",
                        language: Some("en"),
                        datatype: None,
                    },
                },
                format: "n_quads",
                graph: statement.graph().map(|term| LegacyTerm {
                    source: term.source(),
                    value: if graph == Some("_:graph") {
                        LegacyValue::BlankNode { label: "graph" }
                    } else {
                        LegacyValue::Iri { value: "urn:graph" }
                    },
                }),
            })
            .unwrap();
            let provenance = FactId::from_bytes([4; 20]);
            let derivation = vec![FactRef::File(statement.source().span().file())];
            let identity = serde_json::to_vec(&(
                RDF_STATEMENT_NAMESPACE,
                "2",
                statement.source().repository(),
                statement.source().generation(),
                provenance,
                statement.source(),
                &payload,
                &derivation,
            ))
            .unwrap();
            let envelope = new_rdf_statement_envelope(provenance, &statement).unwrap();
            assert_eq!(envelope.version, "2");
            assert_eq!(envelope.payload.as_bytes(), payload.as_bytes());
            assert_eq!(
                envelope.id,
                derive_fact("rootlight.rdf_statement/v2", &identity).id()
            );
        }
    }

    #[test]
    fn dgraph_source_ownership_bounds_and_payload_limit_stay_authenticated() {
        let text = "<alice><status>\"λ\".";
        let (source, spans) = source_spans(text, ["<alice>", "<status>", "\"λ\""]);
        for invalid in [
            SourceSpan::new(
                FileId::from_bytes([8; 20]),
                spans[2].start_byte(),
                spans[2].end_byte(),
            )
            .unwrap(),
            SourceSpan::new(
                spans[2].file(),
                spans[2].start_byte(),
                spans[2].start_byte(),
            )
            .unwrap(),
            spans[1],
            SourceSpan::new(
                spans[2].file(),
                spans[2].start_byte(),
                spans[2].end_byte() + 3,
            )
            .unwrap(),
            SourceSpan::new(
                spans[2].file(),
                spans[2].start_byte(),
                spans[2].start_byte() + 2,
            )
            .unwrap(),
        ] {
            assert!(
                RdfStatement::from_nquads_dgraph(
                    source.clone(),
                    text,
                    [spans[0], spans[1], invalid]
                )
                .is_err()
            );
        }
        for suffix in ["", ".\n", ". #comment", ". ."] {
            let text = format!("<alice><status>\"value\"{suffix}");
            assert!(dgraph(&text, ["<alice>", "<status>", "\"value\""]).is_err());
        }
        let literal = format!("\"{}\"", "x".repeat(MAX_RDF_STATEMENT_PAYLOAD_BYTES / 3));
        let text = format!("<alice><status>{literal}.");
        let statement = dgraph(&text, ["<alice>", "<status>", &literal]).unwrap();
        assert!(new_rdf_statement_envelope(FactId::from_bytes([4; 20]), &statement).is_err());
        let huge = "x".repeat(MAX_RDF_STATEMENT_PAYLOAD_BYTES + 1);
        assert_eq!(
            RdfStatement::from_nquads_dgraph(source, &huge, spans),
            Err(DgraphStatementError::InvalidEvidence)
        );
    }
}
