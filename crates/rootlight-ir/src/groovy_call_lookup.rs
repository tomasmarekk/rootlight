//! Source-owned Groovy call, value and static-import lookup inputs.
//! These records retain syntax and lexical context; they do not select an overload
//! or authenticate runtime dispatch. File-scoped imports are shared across uses.

use std::io::{self, Write};

use rootlight_ids::{ContentHash, FactId, GenerationId, RepositoryId, content_hash, derive_fact};
use serde::{Deserialize, Serialize};

use crate::{
    ExtensionCriticality, ExtensionEnvelope, FactEvidence, FactRef, SourceRef, SourceSpan,
    SourceTextEncoding,
};

/// Namespace for complete native Groovy call lookup context.
pub const GROOVY_CALL_LOOKUP_NAMESPACE: &str = "rootlight.groovy_call_lookup";
/// Namespace for source-owned, file-scoped Groovy static imports.
pub const GROOVY_STATIC_IMPORT_NAMESPACE: &str = "rootlight.groovy_static_import";
/// Namespace for source-owned Groovy value lookup context.
pub const GROOVY_VALUE_LOOKUP_NAMESPACE: &str = "rootlight.groovy_value_lookup";

/// Native write form, not a claim that an assignment is semantically valid.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GroovyValueWrite {
    /// A bare variable on the left of ordinary `=`.
    Assignment,
    /// A compound or Elvis assignment requiring additional read/write semantics.
    CompoundAssignment,
    /// A prefix or postfix increment/decrement.
    Update,
    /// A parenthesized or list target, not the compiler's bare variable rewrite.
    Destructuring,
}

/// Native lexical boundaries for one written variable expression.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GroovyValueLookup {
    /// Exact written identifier; its span is the envelope's complete source.
    pub spelling: String,
    /// Hash of the identifier, matching the reference occurrence.
    pub name_hash: ContentHash,
    /// Nearest authored type, absent at script level.
    pub enclosing_type: Option<SourceSpan>,
    /// Nearest method or constructor, excluding closures.
    pub enclosing_callable: Option<SourceSpan>,
    /// A local or parameter binding exists, even when its symbol is unavailable.
    pub lexical_binding: bool,
    /// Whether the native enclosing context is static.
    pub static_context: bool,
    /// False for assignment/update targets, whose accessor semantics differ.
    pub read_only: bool,
    /// Native write classification; omitted legacy evidence remains unclassified.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub write_kind: Option<GroovyValueWrite>,
}

/// Encodes a native variable expression without selecting a field or accessor.
///
/// # Errors
/// Rejects foreign identity, inconsistent spelling/hash or noncontained owners.
pub fn new_groovy_value_lookup_envelope(
    repository: RepositoryId,
    generation: GenerationId,
    provenance: FactId,
    source: SourceRef,
    lookup: GroovyValueLookup,
) -> Result<ExtensionEnvelope, GroovyCallLookupError> {
    new_groovy_value_lookup_envelope_with_encoding(
        repository, generation, provenance, source, lookup, None,
    )
}

/// Encodes value context against physical UTF-16 spans, or the exact legacy UTF-8 form.
/// The encoding is explicit evidence metadata, not permission to reinterpret a source hash.
///
/// # Errors
/// Rejects inconsistent spelling widths, unaligned spans and invalid legacy context.
pub fn new_groovy_value_lookup_envelope_with_encoding(
    repository: RepositoryId,
    generation: GenerationId,
    provenance: FactId,
    source: SourceRef,
    lookup: GroovyValueLookup,
    encoding: Option<SourceTextEncoding>,
) -> Result<ExtensionEnvelope, GroovyCallLookupError> {
    let span = source.span();
    if (lookup.read_only && lookup.write_kind.is_some())
        || lookup.spelling.is_empty()
        || spelling_width(&lookup.spelling, encoding)
            != span.end_byte().checked_sub(span.start_byte())
        || content_hash(lookup.spelling.as_bytes()) != lookup.name_hash
        || lookup
            .enclosing_type
            .is_some_and(|owner| !contains(owner, span))
        || lookup
            .enclosing_callable
            .is_some_and(|owner| !contains(owner, span))
        || !aligned_spans(
            encoding,
            [Some(span), lookup.enclosing_type, lookup.enclosing_callable],
        )
    {
        return Err(GroovyCallLookupError);
    }
    encode(
        GROOVY_VALUE_LOOKUP_NAMESPACE,
        repository,
        generation,
        provenance,
        source,
        &lookup,
        encoding,
    )
}

/// Decodes bounded value context and verifies its complete canonical envelope.
///
/// # Errors
/// Rejects altered, oversized, unknown-version or noncanonical evidence.
pub fn decode_groovy_value_lookup_envelope(
    envelope: &ExtensionEnvelope,
) -> Result<GroovyValueLookup, GroovyCallLookupError> {
    decode_groovy_value_lookup_envelope_with_encoding(envelope).map(|(lookup, _)| lookup)
}

/// Verifies value context and returns its authenticated physical source encoding.
///
/// # Errors
/// Rejects altered, oversized, unknown-version or noncanonical evidence.
pub fn decode_groovy_value_lookup_envelope_with_encoding(
    envelope: &ExtensionEnvelope,
) -> Result<(GroovyValueLookup, Option<SourceTextEncoding>), GroovyCallLookupError> {
    let (lookup, encoding): (GroovyValueLookup, _) =
        decode(envelope, GROOVY_VALUE_LOOKUP_NAMESPACE)?;
    let expected = new_groovy_value_lookup_envelope_with_encoding(
        envelope.repository,
        envelope.generation,
        envelope.provenance,
        envelope
            .evidence
            .source
            .clone()
            .ok_or(GroovyCallLookupError)?,
        lookup.clone(),
        encoding,
    )?;
    if expected != *envelope {
        return Err(GroovyCallLookupError);
    }
    Ok((lookup, encoding))
}

/// Native argument groups without guessed types or spread expansion.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GroovyCallArguments {
    /// Ordinary arguments, including an explicitly written map or closure literal.
    pub positional: u32,
    /// Named entries; the compiler groups these into one map argument.
    pub named: u32,
    /// Spread expressions whose expansion length is unknown.
    pub spread: u32,
    /// Closures appended after the parenthesized argument list or callable name.
    pub trailing_closures: u32,
}

/// Lookup inputs attached to the full source of one implicit call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GroovyCallLookup {
    /// Exact native terminal identifier, not the full invocation prefix.
    pub name: SourceSpan,
    /// Hash of the written terminal identifier, matching the occurrence name.
    pub name_hash: ContentHash,
    /// Written identifier retained for language-defined accessor lookup.
    pub spelling: String,
    /// Native argument groups from the complete capture plan.
    pub arguments: GroovyCallArguments,
    /// Nearest authored enclosing type; absent for a script-level call.
    pub enclosing_type: Option<SourceSpan>,
    /// Nearest method or constructor declaration, excluding closure declarations.
    pub enclosing_callable: Option<SourceSpan>,
    /// A visible local or parameter binding exists, even if its symbol is unavailable.
    pub lexical_binding: bool,
    /// The native enclosing method, initializer or field has static context.
    pub static_context: bool,
}

/// A native static import, stored once for the compilation unit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GroovyStaticImport {
    /// Exact qualified-name field within the complete import declaration source.
    pub name: SourceSpan,
    /// Exact authored alias, when present.
    pub alias: Option<SourceSpan>,
    /// Hash of the canonical imported owner type path.
    pub owner: ContentHash,
    /// Original member name hash; absent only for a static star import.
    pub member: Option<ContentHash>,
    /// Visible alias or member name hash; absent only for a static star import.
    pub binding: Option<ContentHash>,
    /// Canonical member spelling, absent for a star import.
    pub member_spelling: Option<String>,
    /// Written alias or canonical member spelling, absent for a star import.
    pub binding_spelling: Option<String>,
}

/// Invalid source ownership, argument counts or canonical call/import encoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("invalid groovy call lookup evidence")]
pub struct GroovyCallLookupError;

/// Encodes bounded lookup context for a source-owned implicit invocation.
///
/// # Errors
/// Rejects inconsistent identities, noncontained names/owners and impossible
/// argument counts. The parser must independently prove complete native capture.
pub fn new_groovy_call_lookup_envelope(
    repository: RepositoryId,
    generation: GenerationId,
    provenance: FactId,
    source: SourceRef,
    lookup: GroovyCallLookup,
) -> Result<ExtensionEnvelope, GroovyCallLookupError> {
    new_groovy_call_lookup_envelope_with_encoding(
        repository, generation, provenance, source, lookup, None,
    )
}

/// Encodes call context with encoding-aware physical identifier widths.
/// `None` retains the byte-for-byte version-one representation.
///
/// # Errors
/// Rejects invalid ownership, spelling, alignment, argument counts or bounded payloads.
pub fn new_groovy_call_lookup_envelope_with_encoding(
    repository: RepositoryId,
    generation: GenerationId,
    provenance: FactId,
    source: SourceRef,
    lookup: GroovyCallLookup,
    encoding: Option<SourceTextEncoding>,
) -> Result<ExtensionEnvelope, GroovyCallLookupError> {
    let span = source.span();
    let arguments = lookup.arguments;
    let count = u64::from(arguments.positional)
        + u64::from(arguments.named)
        + u64::from(arguments.spread)
        + u64::from(arguments.trailing_closures);
    if !contains(span, lookup.name)
        || lookup.spelling.is_empty()
        || spelling_width(&lookup.spelling, encoding)
            != lookup.name.end_byte().checked_sub(lookup.name.start_byte())
        || content_hash(lookup.spelling.as_bytes()) != lookup.name_hash
        || count > span.end_byte() - span.start_byte()
        || lookup
            .enclosing_type
            .is_some_and(|owner| !contains(owner, span))
        || lookup
            .enclosing_callable
            .is_some_and(|owner| !contains(owner, span))
        || !aligned_spans(
            encoding,
            [
                Some(span),
                Some(lookup.name),
                lookup.enclosing_type,
                lookup.enclosing_callable,
            ],
        )
    {
        return Err(GroovyCallLookupError);
    }
    encode(
        GROOVY_CALL_LOOKUP_NAMESPACE,
        repository,
        generation,
        provenance,
        source,
        &lookup,
        encoding,
    )
}

/// Encodes a source-owned static import independently of its consumers.
///
/// # Errors
/// Rejects foreign identities, noncontained or overlapping names/aliases and
/// inconsistent star/member binding shapes. Canonical type spelling is parser-owned.
pub fn new_groovy_static_import_envelope(
    repository: RepositoryId,
    generation: GenerationId,
    provenance: FactId,
    source: SourceRef,
    import: GroovyStaticImport,
) -> Result<ExtensionEnvelope, GroovyCallLookupError> {
    new_groovy_static_import_envelope_with_encoding(
        repository, generation, provenance, source, import, None,
    )
}

/// Encodes a static import with physical UTF-16 name and alias coordinates.
/// Hashes continue to describe decoded language-defined spellings.
///
/// # Errors
/// Rejects invalid import shape, spelling widths, source ownership or alignment.
pub fn new_groovy_static_import_envelope_with_encoding(
    repository: RepositoryId,
    generation: GenerationId,
    provenance: FactId,
    source: SourceRef,
    import: GroovyStaticImport,
    encoding: Option<SourceTextEncoding>,
) -> Result<ExtensionEnvelope, GroovyCallLookupError> {
    if !contains(source.span(), import.name)
        || import.alias.is_some_and(|alias| {
            !contains(source.span(), alias) || alias.start_byte() < import.name.end_byte()
        })
        || import.member.is_some() != import.binding.is_some()
        || (import.member.is_none() && import.alias.is_some())
        || import.member
            != import
                .member_spelling
                .as_deref()
                .map(|name| content_hash(name.as_bytes()))
        || import.binding
            != import
                .binding_spelling
                .as_deref()
                .map(|name| content_hash(name.as_bytes()))
        || import.member_spelling.as_deref().is_some_and(|name| {
            name.is_empty()
                || spelling_width(name, encoding)
                    .is_none_or(|length| length > import.name.end_byte() - import.name.start_byte())
        })
        || import.binding_spelling.as_deref().is_some_and(|name| {
            name.is_empty()
                || import.alias.is_some_and(|span| {
                    spelling_width(name, encoding) != span.end_byte().checked_sub(span.start_byte())
                })
        })
        || (import.alias.is_none() && import.member_spelling != import.binding_spelling)
        || !aligned_spans(
            encoding,
            [Some(source.span()), Some(import.name), import.alias],
        )
    {
        return Err(GroovyCallLookupError);
    }
    encode(
        GROOVY_STATIC_IMPORT_NAMESPACE,
        repository,
        generation,
        provenance,
        source,
        &import,
        encoding,
    )
}

/// Verifies the canonical identity and ownership of native call lookup evidence.
///
/// # Errors
/// Rejects altered, oversized, unknown-version or noncanonical envelopes.
pub fn decode_groovy_call_lookup_envelope(
    envelope: &ExtensionEnvelope,
) -> Result<GroovyCallLookup, GroovyCallLookupError> {
    decode_groovy_call_lookup_envelope_with_encoding(envelope).map(|(lookup, _)| lookup)
}

/// Verifies call context and returns its authenticated physical source encoding.
///
/// # Errors
/// Rejects altered, oversized, unknown-version or noncanonical envelopes.
pub fn decode_groovy_call_lookup_envelope_with_encoding(
    envelope: &ExtensionEnvelope,
) -> Result<(GroovyCallLookup, Option<SourceTextEncoding>), GroovyCallLookupError> {
    let (lookup, encoding): (GroovyCallLookup, _) = decode(envelope, GROOVY_CALL_LOOKUP_NAMESPACE)?;
    let expected = new_groovy_call_lookup_envelope_with_encoding(
        envelope.repository,
        envelope.generation,
        envelope.provenance,
        envelope
            .evidence
            .source
            .clone()
            .ok_or(GroovyCallLookupError)?,
        lookup.clone(),
        encoding,
    )?;
    if expected != *envelope {
        return Err(GroovyCallLookupError);
    }
    Ok((lookup, encoding))
}

/// Verifies the canonical identity and ownership of a static import.
///
/// # Errors
/// Rejects altered, oversized, unknown-version or noncanonical envelopes.
pub fn decode_groovy_static_import_envelope(
    envelope: &ExtensionEnvelope,
) -> Result<GroovyStaticImport, GroovyCallLookupError> {
    decode_groovy_static_import_envelope_with_encoding(envelope).map(|(import, _)| import)
}

/// Verifies an import and returns its authenticated physical source encoding.
///
/// # Errors
/// Rejects altered, oversized, unknown-version or noncanonical envelopes.
pub fn decode_groovy_static_import_envelope_with_encoding(
    envelope: &ExtensionEnvelope,
) -> Result<(GroovyStaticImport, Option<SourceTextEncoding>), GroovyCallLookupError> {
    let (import, encoding): (GroovyStaticImport, _) =
        decode(envelope, GROOVY_STATIC_IMPORT_NAMESPACE)?;
    let expected = new_groovy_static_import_envelope_with_encoding(
        envelope.repository,
        envelope.generation,
        envelope.provenance,
        envelope
            .evidence
            .source
            .clone()
            .ok_or(GroovyCallLookupError)?,
        import.clone(),
        encoding,
    )?;
    if expected != *envelope {
        return Err(GroovyCallLookupError);
    }
    Ok((import, encoding))
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct EncodedLookup<T> {
    source_encoding: SourceTextEncoding,
    lookup: T,
}

fn encode(
    namespace: &str,
    repository: RepositoryId,
    generation: GenerationId,
    provenance: FactId,
    source: SourceRef,
    value: &impl Serialize,
    encoding: Option<SourceTextEncoding>,
) -> Result<ExtensionEnvelope, GroovyCallLookupError> {
    if source.repository() != repository || source.generation() != generation {
        return Err(GroovyCallLookupError);
    }
    let (version, domain, payload) = match encoding {
        None => (
            "1",
            "rootlight.groovy_call_context/v1",
            bounded_payload(value),
        ),
        Some(source_encoding) => (
            "2",
            "rootlight.groovy_call_context/v2",
            bounded_payload(&EncodedLookup {
                source_encoding,
                lookup: value,
            }),
        ),
    };
    let payload = payload.map_err(|_| GroovyCallLookupError)?;
    if payload.len() > 4096 {
        return Err(GroovyCallLookupError);
    }
    let subject = FactRef::File(source.span().file());
    let identity = serde_json::to_vec(&(
        namespace, version, repository, generation, provenance, &source, &payload, subject,
    ))
    .map_err(|_| GroovyCallLookupError)?;
    Ok(ExtensionEnvelope {
        id: derive_fact(domain, &identity).id(),
        repository,
        generation,
        namespace: namespace.to_owned(),
        version: version.to_owned(),
        criticality: ExtensionCriticality::Noncritical,
        payload,
        provenance,
        evidence: FactEvidence {
            source: Some(source),
            derivation: vec![subject],
        },
    })
}

fn decode<T: serde::de::DeserializeOwned>(
    envelope: &ExtensionEnvelope,
    namespace: &str,
) -> Result<(T, Option<SourceTextEncoding>), GroovyCallLookupError> {
    if envelope.namespace != namespace || envelope.payload.len() > 4096 {
        return Err(GroovyCallLookupError);
    }
    match envelope.version.as_str() {
        "1" => serde_json::from_str(&envelope.payload)
            .map(|lookup| (lookup, None))
            .map_err(|_| GroovyCallLookupError),
        "2" => serde_json::from_str::<EncodedLookup<T>>(&envelope.payload)
            .map(|encoded| (encoded.lookup, Some(encoded.source_encoding)))
            .map_err(|_| GroovyCallLookupError),
        _ => Err(GroovyCallLookupError),
    }
}

fn spelling_width(spelling: &str, encoding: Option<SourceTextEncoding>) -> Option<u64> {
    if spelling.len() > 4096 || (encoding.is_some() && spelling.contains('\0')) {
        return None;
    }
    match encoding {
        None => u64::try_from(spelling.len()).ok(),
        Some(_) => u64::try_from(spelling.encode_utf16().count())
            .ok()?
            .checked_mul(2),
    }
}

fn bounded_payload(value: &impl Serialize) -> Result<String, GroovyCallLookupError> {
    // Reject escaped JSON expansion before an input-sized serialized allocation.
    let mut writer = PayloadWriter {
        bytes: [0; 4096],
        length: 0,
    };
    serde_json::to_writer(&mut writer, value).map_err(|_| GroovyCallLookupError)?;
    String::from_utf8(writer.bytes[..writer.length].to_vec()).map_err(|_| GroovyCallLookupError)
}

struct PayloadWriter {
    bytes: [u8; 4096],
    length: usize,
}

impl Write for PayloadWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let end = self
            .length
            .checked_add(bytes.len())
            .ok_or_else(|| io::Error::other("groovy lookup payload limit"))?;
        let target = self
            .bytes
            .get_mut(self.length..end)
            .ok_or_else(|| io::Error::other("groovy lookup payload limit"))?;
        target.copy_from_slice(bytes);
        self.length = end;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn aligned_spans(
    encoding: Option<SourceTextEncoding>,
    spans: impl IntoIterator<Item = Option<SourceSpan>>,
) -> bool {
    encoding.is_none()
        || spans
            .into_iter()
            .flatten()
            .all(|span| span.start_byte().is_multiple_of(2) && span.end_byte().is_multiple_of(2))
}

fn contains(outer: SourceSpan, inner: SourceSpan) -> bool {
    outer.file() == inner.file()
        && outer.start_byte() <= inner.start_byte()
        && inner.start_byte() < inner.end_byte()
        && inner.end_byte() <= outer.end_byte()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rootlight_ids::{FileId, content_hash};

    fn span(start: u64, end: u64) -> SourceSpan {
        SourceSpan::new(FileId::from_bytes([3; 20]), start, end).unwrap()
    }

    fn source() -> SourceRef {
        SourceRef::new(
            RepositoryId::from_bytes([1; 16]),
            GenerationId::from_bytes([2; 20]),
            span(5, 30),
            content_hash(b"source"),
            None,
        )
    }

    fn lookup() -> GroovyCallLookup {
        GroovyCallLookup {
            name: span(5, 9),
            name_hash: content_hash(b"fold"),
            spelling: "fold".into(),
            arguments: GroovyCallArguments {
                positional: 1,
                named: 2,
                spread: 0,
                trailing_closures: 1,
            },
            enclosing_type: Some(span(0, 100)),
            enclosing_callable: Some(span(3, 50)),
            lexical_binding: false,
            static_context: false,
        }
    }

    fn import() -> GroovyStaticImport {
        GroovyStaticImport {
            name: span(10, 18),
            alias: Some(span(22, 30)),
            owner: content_hash(b"sample.Tools"),
            member: Some(content_hash(b"fold")),
            binding: Some(content_hash(b"selected")),
            member_spelling: Some("fold".into()),
            binding_spelling: Some("selected".into()),
        }
    }

    #[test]
    fn value_write_classification_preserves_legacy_unknown_and_rejects_conflicting_reads() {
        let source = SourceRef::new(
            source().repository(),
            source().generation(),
            span(5, 9),
            source().content_hash(),
            None,
        );
        let mut lookup = GroovyValueLookup {
            spelling: "fold".into(),
            name_hash: content_hash(b"fold"),
            enclosing_type: Some(span(0, 100)),
            enclosing_callable: Some(span(3, 50)),
            lexical_binding: false,
            static_context: true,
            read_only: false,
            write_kind: None,
        };
        let encode = |value| {
            new_groovy_value_lookup_envelope(
                source.repository(),
                source.generation(),
                FactId::from_bytes([4; 20]),
                source.clone(),
                value,
            )
        };
        let legacy = encode(lookup.clone()).unwrap();
        assert!(!legacy.payload.contains("write_kind"));
        assert_eq!(
            decode_groovy_value_lookup_envelope(&legacy)
                .unwrap()
                .write_kind,
            None
        );
        for write in [
            GroovyValueWrite::Assignment,
            GroovyValueWrite::CompoundAssignment,
            GroovyValueWrite::Update,
            GroovyValueWrite::Destructuring,
        ] {
            lookup.write_kind = Some(write);
            let classified = encode(lookup.clone()).unwrap();
            assert_eq!(
                decode_groovy_value_lookup_envelope(&classified).unwrap(),
                lookup
            );
            assert_ne!(classified.id, legacy.id);
            let mut forged = legacy.clone();
            forged.payload = classified.payload;
            assert!(decode_groovy_value_lookup_envelope(&forged).is_err());
            let mut contradictory = lookup.clone();
            contradictory.read_only = true;
            assert!(encode(contradictory).is_err());
        }
    }

    #[test]
    fn value_lookup_rejects_changed_source_and_canonical_identity() {
        let source = SourceRef::new(
            source().repository(),
            source().generation(),
            span(5, 9),
            source().content_hash(),
            None,
        );
        let lookup = GroovyValueLookup {
            spelling: "fold".into(),
            name_hash: content_hash(b"fold"),
            enclosing_type: Some(span(0, 100)),
            enclosing_callable: Some(span(3, 50)),
            lexical_binding: false,
            static_context: true,
            read_only: true,
            write_kind: None,
        };
        let encode = |value| {
            new_groovy_value_lookup_envelope(
                source.repository(),
                source.generation(),
                FactId::from_bytes([4; 20]),
                source.clone(),
                value,
            )
        };
        let envelope = encode(lookup.clone()).unwrap();
        assert_eq!(
            decode_groovy_value_lookup_envelope(&envelope).unwrap(),
            lookup
        );
        for mutate in [
            |value: &mut ExtensionEnvelope| value.payload.push(' '),
            |value: &mut ExtensionEnvelope| value.version = "2".into(),
            |value: &mut ExtensionEnvelope| value.id = FactId::from_bytes([99; 20]),
            |value: &mut ExtensionEnvelope| value.evidence.derivation.clear(),
            |value: &mut ExtensionEnvelope| value.payload = "x".repeat(4097),
        ] {
            let mut changed = envelope.clone();
            mutate(&mut changed);
            assert!(decode_groovy_value_lookup_envelope(&changed).is_err());
        }
        for mutate in [
            |value: &mut GroovyValueLookup| value.spelling.push('x'),
            |value: &mut GroovyValueLookup| value.name_hash = content_hash(b"other"),
            |value: &mut GroovyValueLookup| value.enclosing_type = Some(span(6, 100)),
            |value: &mut GroovyValueLookup| value.enclosing_callable = Some(span(3, 8)),
        ] {
            let mut changed = lookup.clone();
            mutate(&mut changed);
            assert!(encode(changed).is_err());
        }
    }

    fn call_envelope(value: GroovyCallLookup) -> Result<ExtensionEnvelope, GroovyCallLookupError> {
        let source = source();
        new_groovy_call_lookup_envelope(
            source.repository(),
            source.generation(),
            FactId::from_bytes([4; 20]),
            source,
            value,
        )
    }

    fn import_envelope(
        value: GroovyStaticImport,
    ) -> Result<ExtensionEnvelope, GroovyCallLookupError> {
        let source = source();
        new_groovy_static_import_envelope(
            source.repository(),
            source.generation(),
            FactId::from_bytes([4; 20]),
            source,
            value,
        )
    }

    #[test]
    fn call_and_import_context_reject_tampering_and_unknown_encoding() {
        let call = call_envelope(lookup()).unwrap();
        let import = import_envelope(import()).unwrap();
        assert_eq!(decode_groovy_call_lookup_envelope(&call).unwrap(), lookup());
        for original in [call, import] {
            let valid = |value: &ExtensionEnvelope| {
                if original.namespace == GROOVY_CALL_LOOKUP_NAMESPACE {
                    decode_groovy_call_lookup_envelope(value).is_ok()
                } else {
                    decode_groovy_static_import_envelope(value).is_ok()
                }
            };
            assert!(valid(&original));
            for mutate in [
                |e: &mut ExtensionEnvelope| e.payload.push(' '),
                |e: &mut ExtensionEnvelope| e.payload = " ".repeat(4097),
                |e: &mut ExtensionEnvelope| e.version = "2".into(),
                |e: &mut ExtensionEnvelope| e.namespace.push('x'),
                |e: &mut ExtensionEnvelope| e.id = FactId::from_bytes([9; 20]),
                |e: &mut ExtensionEnvelope| e.provenance = FactId::from_bytes([9; 20]),
                |e: &mut ExtensionEnvelope| e.evidence.source = None,
                |e: &mut ExtensionEnvelope| e.evidence.derivation.clear(),
                |e: &mut ExtensionEnvelope| e.repository = RepositoryId::from_bytes([9; 16]),
                |e: &mut ExtensionEnvelope| e.generation = GenerationId::from_bytes([9; 20]),
            ] {
                let mut changed = original.clone();
                mutate(&mut changed);
                assert!(!valid(&changed));
            }
        }
    }

    #[test]
    fn call_context_requires_contained_names_owners_and_bounded_counts() {
        for mutate in [
            |v: &mut GroovyCallLookup| v.name = span(1, 4),
            |v: &mut GroovyCallLookup| v.name = span(8, 8),
            |v: &mut GroovyCallLookup| {
                v.name = SourceSpan::new(FileId::from_bytes([9; 20]), 5, 9).unwrap()
            },
            |v: &mut GroovyCallLookup| v.enclosing_type = Some(span(6, 30)),
            |v: &mut GroovyCallLookup| v.enclosing_callable = Some(span(5, 29)),
            |v: &mut GroovyCallLookup| v.arguments.spread = u32::MAX,
            |v: &mut GroovyCallLookup| v.spelling = "different".into(),
            |v: &mut GroovyCallLookup| v.name_hash = content_hash(b"other"),
        ] {
            let mut value = lookup();
            mutate(&mut value);
            assert!(call_envelope(value).is_err());
        }
        let mut script = lookup();
        script.enclosing_type = None;
        script.enclosing_callable = None;
        script.arguments = GroovyCallArguments::default();
        assert!(call_envelope(script).is_ok());
    }

    #[test]
    fn static_import_context_requires_exact_member_or_star_shape() {
        for mutate in [
            |v: &mut GroovyStaticImport| v.name = span(1, 4),
            |v: &mut GroovyStaticImport| v.alias = Some(span(17, 20)),
            |v: &mut GroovyStaticImport| v.member = None,
            |v: &mut GroovyStaticImport| v.binding = None,
            |v: &mut GroovyStaticImport| v.member_spelling = Some("other".into()),
            |v: &mut GroovyStaticImport| v.binding_spelling = Some("other".into()),
            |v: &mut GroovyStaticImport| {
                v.member = None;
                v.binding = None;
            },
        ] {
            let mut value = import();
            mutate(&mut value);
            assert!(import_envelope(value).is_err());
        }
        let mut star = import();
        star.alias = None;
        star.member = None;
        star.binding = None;
        star.member_spelling = None;
        star.binding_spelling = None;
        assert!(import_envelope(star).is_ok());
    }

    #[test]
    fn call_context_encoding_keeps_the_existing_payload_bound() {
        let mut value = lookup();
        value.spelling = "x".repeat(4096);
        value.name_hash = content_hash(value.spelling.as_bytes());
        value.name = span(0, 4096);
        value.enclosing_type = None;
        value.enclosing_callable = None;
        value.arguments = GroovyCallArguments::default();
        let source = SourceRef::new(
            source().repository(),
            source().generation(),
            span(0, 4098),
            content_hash(b"source"),
            None,
        );
        assert!(
            new_groovy_call_lookup_envelope(
                source.repository(),
                source.generation(),
                FactId::from_bytes([4; 20]),
                source,
                value
            )
            .is_err()
        );
    }

    #[test]
    fn encoded_call_value_and_alias_use_physical_widths_and_preserve_legacy() {
        for encoding in [SourceTextEncoding::Utf16Le, SourceTextEncoding::Utf16Be] {
            let physical = SourceRef::new(
                source().repository(),
                source().generation(),
                span(2, 80),
                source().content_hash(),
                None,
            );
            let mut call = lookup();
            call.name = span(2, 10);
            call.enclosing_callable = Some(span(0, 100));
            let encoded = new_groovy_call_lookup_envelope_with_encoding(
                physical.repository(),
                physical.generation(),
                FactId::from_bytes([4; 20]),
                physical.clone(),
                call.clone(),
                Some(encoding),
            )
            .unwrap();
            assert_eq!(encoded.version, "2");
            assert_eq!(
                decode_groovy_call_lookup_envelope_with_encoding(&encoded).unwrap(),
                (call.clone(), Some(encoding))
            );
            assert_eq!(decode_groovy_call_lookup_envelope(&encoded).unwrap(), call);
            assert!(
                new_groovy_call_lookup_envelope(
                    physical.repository(),
                    physical.generation(),
                    FactId::from_bytes([4; 20]),
                    physical.clone(),
                    call,
                )
                .is_err()
            );

            let value = GroovyValueLookup {
                spelling: "xé𐐀".into(),
                name_hash: content_hash("xé𐐀".as_bytes()),
                enclosing_type: Some(span(0, 100)),
                enclosing_callable: None,
                lexical_binding: false,
                static_context: false,
                read_only: true,
                write_kind: None,
            };
            let name_source = SourceRef::new(
                physical.repository(),
                physical.generation(),
                span(2, 10),
                physical.content_hash(),
                None,
            );
            let encoded_value = new_groovy_value_lookup_envelope_with_encoding(
                physical.repository(),
                physical.generation(),
                FactId::from_bytes([4; 20]),
                name_source,
                value.clone(),
                Some(encoding),
            )
            .unwrap();
            assert_eq!(
                decode_groovy_value_lookup_envelope_with_encoding(&encoded_value).unwrap(),
                (value, Some(encoding))
            );

            let mut imported = import();
            imported.name = span(10, 26);
            imported.alias = Some(span(28, 44));
            let encoded_import = new_groovy_static_import_envelope_with_encoding(
                physical.repository(),
                physical.generation(),
                FactId::from_bytes([4; 20]),
                physical.clone(),
                imported.clone(),
                Some(encoding),
            )
            .unwrap();
            assert_eq!(
                decode_groovy_static_import_envelope_with_encoding(&encoded_import).unwrap(),
                (imported, Some(encoding))
            );
            for original in [encoded, encoded_value, encoded_import] {
                for mutate in [
                    |value: &mut ExtensionEnvelope| value.version = "1".into(),
                    |value: &mut ExtensionEnvelope| value.version = "3".into(),
                    |value: &mut ExtensionEnvelope| value.payload.push(' '),
                    |value: &mut ExtensionEnvelope| {
                        value.payload = value.payload.replace("utf-16", "utf-8")
                    },
                    |value: &mut ExtensionEnvelope| value.id = FactId::from_bytes([99; 20]),
                ] {
                    let mut forged = original.clone();
                    mutate(&mut forged);
                    let valid = match forged.namespace.as_str() {
                        GROOVY_CALL_LOOKUP_NAMESPACE => {
                            decode_groovy_call_lookup_envelope(&forged).is_ok()
                        }
                        GROOVY_VALUE_LOOKUP_NAMESPACE => {
                            decode_groovy_value_lookup_envelope(&forged).is_ok()
                        }
                        GROOVY_STATIC_IMPORT_NAMESPACE => {
                            decode_groovy_static_import_envelope(&forged).is_ok()
                        }
                        _ => unreachable!(),
                    };
                    assert!(!valid);
                }
            }
        }
        let legacy = call_envelope(lookup()).unwrap();
        assert_eq!(
            legacy,
            new_groovy_call_lookup_envelope_with_encoding(
                legacy.repository,
                legacy.generation,
                legacy.provenance,
                legacy.evidence.source.clone().unwrap(),
                lookup(),
                None,
            )
            .unwrap()
        );
        assert_eq!(legacy.payload, serde_json::to_string(&lookup()).unwrap());
    }

    #[test]
    fn encoded_lookup_rejects_odd_owner_endpoints_and_wrong_identifier_widths() {
        let source = SourceRef::new(
            source().repository(),
            source().generation(),
            span(2, 80),
            source().content_hash(),
            None,
        );
        let mut lookup = lookup();
        lookup.name = span(2, 10);
        lookup.enclosing_callable = Some(span(0, 100));
        let encode = |value| {
            new_groovy_call_lookup_envelope_with_encoding(
                source.repository(),
                source.generation(),
                FactId::from_bytes([4; 20]),
                source.clone(),
                value,
                Some(SourceTextEncoding::Utf16Le),
            )
        };
        assert!(encode(lookup.clone()).is_ok());
        for mutate in [
            |value: &mut GroovyCallLookup| value.name = span(3, 11),
            |value: &mut GroovyCallLookup| value.name = span(2, 6),
            |value: &mut GroovyCallLookup| value.enclosing_type = Some(span(1, 100)),
            |value: &mut GroovyCallLookup| value.enclosing_callable = Some(span(0, 101)),
        ] {
            let mut invalid = lookup.clone();
            mutate(&mut invalid);
            assert!(encode(invalid).is_err());
        }
    }
}
