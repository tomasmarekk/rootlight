//! Join JavaScript receiver certificates to an independent closed source grammar.
//! Physical identity, raw ancestry and materialized lexical owners must agree.

use std::{
    collections::{HashMap, HashSet},
    ops::{ControlFlow, Range},
};

use rootlight_adapter_sdk::{AdapterError, SinkError, SyntaxFact, SyntaxFactKind};
use rootlight_cancel::Cancellation;
use rootlight_ids::{ContentHash, FactId, SymbolId};
use rootlight_ir::{
    BuildContextIdentity, ContainerRef, EntityKind, OccurrenceRecord, OccurrenceRole,
    OccurrenceTarget, SourceRef, SourceSpan, SourceTextEncoding,
};

use tree_sitter::{Node, ParseOptions};

use super::{MaterializedEntity, NativeSourceView};

const PREFIX: &str = "javascript.const_receiver";
const REFERENCE: &str = "javascript.const_receiver.reference";
const CLEAN: &str = "javascript.const_receiver_clean_file.signature";
const BINDING: &str = "javascript.const_receiver_binding.signature";
const TARGET: &str = "javascript.const_receiver_target.signature";
const CALL: &str = "javascript.const_receiver_member_call.signature";
const OWNER: &str = "javascript.const_receiver_owner.signature";
const NONBINDING: &str = "javascript.const_receiver_nonbinding.signature";

/// Identifies the exact native receiver Reference required by the partition.
pub(super) fn required(fact: &SyntaxFact) -> bool {
    fact.kind() == SyntaxFactKind::Occurrence && fact.syntax_kind().as_str() == REFERENCE
}

/// Excludes private proof carriers from ordinary semantic signature input.
pub(super) fn metadata(fact: &SyntaxFact) -> bool {
    fact.kind() == SyntaxFactKind::Signature
        && matches!(
            fact.syntax_kind().as_str(),
            CLEAN | BINDING | TARGET | CALL | OWNER | NONBINDING
        )
}

struct Work {
    remaining: usize,
    exhausted: bool,
}

impl Work {
    fn step(&mut self, cancel: &Cancellation) -> Result<bool, AdapterError> {
        cancel.check()?;
        let Some(next) = self.remaining.checked_sub(1) else {
            self.exhausted = true;
            return Ok(false);
        };
        self.remaining = next;
        Ok(true)
    }
}

#[derive(Clone)]
struct Word {
    range: Range<usize>,
}

#[derive(Clone)]
struct WrittenBinding {
    name: Range<usize>,
    declaration: Range<usize>,
}

#[derive(Clone)]
struct WrittenReceiver {
    name: Range<usize>,
    call: Range<usize>,
    owner: Range<usize>,
}

struct Program<'source> {
    source: &'source [u8],
    words: Vec<Word>,
    native_root: Range<usize>,
    bindings: Vec<WrittenBinding>,
    receivers: Vec<WrittenReceiver>,
    nonbinding: HashSet<Range<usize>>,
}

fn plain_word(raw: &[u8]) -> bool {
    raw.first()
        .is_some_and(|byte| byte.is_ascii_alphabetic() || matches!(byte, b'_' | b'$'))
        && raw
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'$'))
        && raw != b"eval"
}

// IdentifierName selectors remain valid even when the spelling is reserved.
fn reserved(raw: &[u8]) -> bool {
    matches!(
        raw,
        b"await"
            | b"break"
            | b"case"
            | b"catch"
            | b"class"
            | b"const"
            | b"continue"
            | b"debugger"
            | b"default"
            | b"delete"
            | b"do"
            | b"else"
            | b"enum"
            | b"export"
            | b"extends"
            | b"false"
            | b"finally"
            | b"for"
            | b"function"
            | b"if"
            | b"import"
            | b"in"
            | b"instanceof"
            | b"let"
            | b"new"
            | b"null"
            | b"return"
            | b"super"
            | b"switch"
            | b"this"
            | b"throw"
            | b"true"
            | b"try"
            | b"typeof"
            | b"var"
            | b"void"
            | b"while"
            | b"with"
            | b"yield"
            | b"implements"
            | b"interface"
            | b"package"
            | b"private"
            | b"protected"
            | b"public"
            | b"static"
    )
}

impl<'source> Program<'source> {
    fn bytes(
        &self,
        node: Node<'_>,
        work: &mut Work,
        cancel: &Cancellation,
    ) -> Result<Option<&'source [u8]>, AdapterError> {
        let Some(bytes) = self.source.get(node.byte_range()) else {
            return Ok(None);
        };
        for _ in bytes.chunks(64) {
            if !work.step(cancel)? {
                return Ok(None);
            }
        }
        Ok(Some(bytes))
    }

    fn leaf(
        &self,
        node: Node<'_>,
        work: &mut Work,
        cancel: &Cancellation,
    ) -> Result<bool, AdapterError> {
        if !work.step(cancel)? {
            return Ok(false);
        }
        let Some(raw) = self.bytes(node, work, cancel)? else {
            return Ok(false);
        };
        Ok(match node.kind() {
            "identifier" => plain_word(raw) && !reserved(raw),
            "true" | "false" | "null" => true,
            "number" => {
                !raw.is_empty()
                    && (raw.len() == 1 || raw.first() != Some(&b'0'))
                    && raw.iter().all(u8::is_ascii_digit)
            }
            "string" => {
                raw.len() >= 2
                    && matches!(raw.first(), Some(b'\'' | b'"'))
                    && raw.first() == raw.last()
                    && raw.get(1..raw.len() - 1).is_some_and(|inside| {
                        inside.iter().all(|byte| {
                            Some(byte) != raw.first() && !matches!(byte, b'\\' | b'\r' | b'\n' | 0)
                        })
                    })
            }
            _ => false,
        })
    }

    fn selector(
        &mut self,
        node: Node<'_>,
        work: &mut Work,
        cancel: &Cancellation,
    ) -> Result<bool, AdapterError> {
        if node.kind() != "property_identifier"
            || !self.bytes(node, work, cancel)?.is_some_and(plain_word)
        {
            return Ok(false);
        }
        self.nonbinding
            .try_reserve(1)
            .map_err(|_| SinkError::AllocationFailed)?;
        self.nonbinding.insert(node.byte_range());
        Ok(true)
    }

    fn arguments(
        &self,
        call: Node<'_>,
        work: &mut Work,
        cancel: &Cancellation,
    ) -> Result<bool, AdapterError> {
        if call.named_child_count() != 2 {
            return Ok(false);
        }
        let Some(arguments) = call
            .child_by_field_name("arguments")
            .filter(|node| node.kind() == "arguments")
        else {
            return Ok(false);
        };
        let mut cursor = call.walk();
        for child in call.children(&mut cursor) {
            if !work.step(cancel)?
                || matches!(child.kind(), "optional_chain" | "?." | "type_arguments")
            {
                return Ok(false);
            }
        }
        let mut cursor = arguments.walk();
        let mut previous = None;
        for child in arguments.children(&mut cursor) {
            if !work.step(cancel)? {
                return Ok(false);
            }
            if child.kind() == "comment" {
                continue;
            }
            if child.is_named() && !self.leaf(child, work, cancel)? {
                return Ok(false);
            }
            if child.kind() == ")" && previous == Some(",") {
                return Ok(false);
            }
            previous = Some(child.kind());
        }
        Ok(true)
    }

    fn initial_value(
        &self,
        node: Node<'_>,
        work: &mut Work,
        cancel: &Cancellation,
    ) -> Result<bool, AdapterError> {
        if node.kind() != "call_expression" {
            return self.leaf(node, work, cancel);
        }
        let Some(callee) = node
            .child_by_field_name("function")
            .filter(|node| node.kind() == "identifier")
        else {
            return Ok(false);
        };
        Ok(self.leaf(callee, work, cancel)? && self.arguments(node, work, cancel)?)
    }

    fn left_side(
        &mut self,
        mut node: Node<'_>,
        work: &mut Work,
        cancel: &Cancellation,
    ) -> Result<bool, AdapterError> {
        loop {
            if !work.step(cancel)? {
                return Ok(false);
            }
            if node.kind() == "identifier" {
                return self.leaf(node, work, cancel);
            }
            if node.kind() != "member_expression" || node.named_child_count() != 2 {
                return Ok(false);
            }
            let (Some(object), Some(property)) = (
                node.child_by_field_name("object"),
                node.child_by_field_name("property"),
            ) else {
                return Ok(false);
            };
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if !work.step(cancel)? || matches!(child.kind(), "optional_chain" | "?.") {
                    return Ok(false);
                }
            }
            if !self.selector(property, work, cancel)? {
                return Ok(false);
            }
            node = object;
        }
    }

    fn member_value(
        &mut self,
        call: Node<'_>,
        owner: &Range<usize>,
        work: &mut Work,
        cancel: &Cancellation,
    ) -> Result<bool, AdapterError> {
        if !work.step(cancel)?
            || call.kind() != "call_expression"
            || !self.arguments(call, work, cancel)?
        {
            return Ok(false);
        }
        let Some(member) = call
            .child_by_field_name("function")
            .filter(|node| node.kind() == "member_expression" && node.named_child_count() == 2)
        else {
            return Ok(false);
        };
        let (Some(receiver), Some(property)) = (
            member
                .child_by_field_name("object")
                .filter(|node| node.kind() == "identifier"),
            member.child_by_field_name("property"),
        ) else {
            return Ok(false);
        };
        let mut cursor = member.walk();
        for child in member.children(&mut cursor) {
            if !work.step(cancel)? || matches!(child.kind(), "optional_chain" | "?.") {
                return Ok(false);
            }
        }
        if !self.leaf(receiver, work, cancel)? || !self.selector(property, work, cancel)? {
            return Ok(false);
        }
        self.receivers
            .try_reserve(1)
            .map_err(|_| SinkError::AllocationFailed)?;
        self.receivers.push(WrittenReceiver {
            name: receiver.byte_range(),
            call: call.byte_range(),
            owner: owner.clone(),
        });
        Ok(true)
    }

    fn object_value(
        &mut self,
        object: Node<'_>,
        owner: &Range<usize>,
        work: &mut Work,
        cancel: &Cancellation,
    ) -> Result<bool, AdapterError> {
        let mut pending = Vec::new();
        pending
            .try_reserve(1)
            .map_err(|_| SinkError::AllocationFailed)?;
        pending.push(object);
        while let Some(object) = pending.pop() {
            if !work.step(cancel)? || object.kind() != "object" {
                return Ok(false);
            }
            let mut cursor = object.walk();
            for child in object.named_children(&mut cursor) {
                if !work.step(cancel)? {
                    return Ok(false);
                }
                if child.kind() == "comment" {
                    continue;
                }
                if child.kind() != "pair" || child.named_child_count() != 2 {
                    return Ok(false);
                }
                let (Some(key), Some(value)) = (
                    child.child_by_field_name("key"),
                    child.child_by_field_name("value"),
                ) else {
                    return Ok(false);
                };
                if key.kind() == "string" {
                    if !self.leaf(key, work, cancel)? {
                        return Ok(false);
                    }
                } else if !self.selector(key, work, cancel)? {
                    return Ok(false);
                }
                match value.kind() {
                    "object" => {
                        pending
                            .try_reserve(1)
                            .map_err(|_| SinkError::AllocationFailed)?;
                        pending.push(value);
                    }
                    "call_expression" => {
                        if !self.member_value(value, owner, work, cancel)? {
                            return Ok(false);
                        }
                    }
                    _ => {
                        if !self.leaf(value, work, cancel)? {
                            return Ok(false);
                        }
                    }
                }
            }
        }
        Ok(true)
    }

    fn admit_hashbang(
        &self,
        node: Node<'_>,
        work: &mut Work,
        cancel: &Cancellation,
    ) -> Result<bool, AdapterError> {
        if node.kind() != "hash_bang_line"
            || node
                .parent()
                .is_none_or(|parent| parent.kind() != "program")
            || !matches!(
                self.source.get(..node.start_byte()),
                Some(b"" | b"\xef\xbb\xbf")
            )
        {
            return Ok(false);
        }
        let Some(raw) = self
            .source
            .get(node.byte_range())
            .and_then(|raw| std::str::from_utf8(raw).ok())
        else {
            return Ok(false);
        };
        let following = self.source.get(node.end_byte()).copied();
        if !raw.starts_with("#!") || !matches!(following, None | Some(b'\n')) {
            return Ok(false);
        }
        // A clean CST alone cannot certify bytes swallowed by this grammar's
        // LF-only hashbang token. Authenticate its actual source line boundary.
        for (at, scalar) in raw.char_indices() {
            if !work.step(cancel)? {
                return Ok(false);
            }
            match scalar {
                '\n' | '\u{2028}' | '\u{2029}' => return Ok(false),
                '\r' if at + 1 != raw.len() || following != Some(b'\n') => return Ok(false),
                _ => {}
            }
        }
        Ok(true)
    }

    fn new(
        source: &'source [u8],
        work: &mut Work,
        cancel: &Cancellation,
    ) -> Result<Option<Self>, AdapterError> {
        cancel.check()?;
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_typescript::LANGUAGE_TSX.into())
            .map_err(|_| super::provider_failure("javascript-const-consumer-grammar"))?;
        let mut progress = |_: &tree_sitter::ParseState| match work.step(cancel) {
            Ok(true) => ControlFlow::Continue(()),
            _ => ControlFlow::Break(()),
        };
        let mut input =
            |offset: usize, _: tree_sitter::Point| source.get(offset..).unwrap_or_default();
        let tree = parser.parse_with_options(
            &mut input,
            None,
            Some(ParseOptions::new().progress_callback(&mut progress)),
        );
        cancel.check()?;
        let Some(tree) = tree.filter(|_| !work.exhausted) else {
            return Ok(None);
        };
        let root = tree.root_node();
        if root.kind() != "program"
            || root.has_error()
            || root.is_missing()
            || root.end_byte() != source.len()
            || !matches!(source.get(..root.start_byte()), Some(b"" | b"\xef\xbb\xbf"))
        {
            return Ok(None);
        }
        let mut result = Self {
            source,
            words: Vec::new(),
            native_root: root.byte_range(),
            bindings: Vec::new(),
            receivers: Vec::new(),
            nonbinding: HashSet::new(),
        };
        let mut cursor = root.walk();
        for statement in root.named_children(&mut cursor) {
            if !work.step(cancel)? {
                return Ok(None);
            }
            match statement.kind() {
                "comment" | "empty_statement" => {}
                "hash_bang_line" => {
                    if !result.admit_hashbang(statement, work, cancel)? {
                        return Ok(None);
                    }
                }
                "lexical_declaration" => {
                    if statement.named_child_count() != 1
                        || statement
                            .child_by_field_name("kind")
                            .is_none_or(|node| node.kind() != "const")
                    {
                        return Ok(None);
                    }
                    let Some(declaration) = statement.named_child(0).filter(|node| {
                        node.kind() == "variable_declarator"
                            && node.child_by_field_name("type").is_none()
                    }) else {
                        return Ok(None);
                    };
                    let (Some(name), Some(value)) = (
                        declaration
                            .child_by_field_name("name")
                            .filter(|node| node.kind() == "identifier"),
                        declaration.child_by_field_name("value"),
                    ) else {
                        return Ok(None);
                    };
                    if !result.leaf(name, work, cancel)?
                        || !result.initial_value(value, work, cancel)?
                    {
                        return Ok(None);
                    }
                    result
                        .bindings
                        .try_reserve(1)
                        .map_err(|_| SinkError::AllocationFailed)?;
                    result.bindings.push(WrittenBinding {
                        name: name.byte_range(),
                        declaration: declaration.byte_range(),
                    });
                }
                "expression_statement" => {
                    let Some(assignment) = statement.named_child(0).filter(|node| {
                        node.kind() == "assignment_expression" && statement.named_child_count() == 1
                    }) else {
                        return Ok(None);
                    };
                    let (Some(left), Some(object)) = (
                        assignment.child_by_field_name("left"),
                        assignment
                            .child_by_field_name("right")
                            .filter(|node| node.kind() == "object"),
                    ) else {
                        return Ok(None);
                    };
                    if assignment.named_child_count() != 2
                        || !result.left_side(left, work, cancel)?
                        || !result.object_value(object, &assignment.byte_range(), work, cancel)?
                    {
                        return Ok(None);
                    }
                }
                _ => return Ok(None),
            }
        }
        let mut cursor = root.walk();
        loop {
            if !work.step(cancel)? {
                return Ok(None);
            }
            let node = cursor.node();
            if matches!(
                node.kind(),
                "identifier"
                    | "property_identifier"
                    | "shorthand_property_identifier"
                    | "shorthand_property_identifier_pattern"
            ) {
                if !result.bytes(node, work, cancel)?.is_some_and(plain_word) {
                    return Ok(None);
                }
                result
                    .words
                    .try_reserve(1)
                    .map_err(|_| SinkError::AllocationFailed)?;
                result.words.push(Word {
                    range: node.byte_range(),
                });
            }
            if cursor.goto_first_child() {
                continue;
            }
            while !cursor.goto_next_sibling() {
                if !cursor.goto_parent() {
                    return Ok(Some(result));
                }
            }
        }
    }

    fn binding(
        &self,
        receiver: &WrittenReceiver,
        work: &mut Work,
        cancel: &Cancellation,
    ) -> Result<Option<&WrittenBinding>, AdapterError> {
        let spelling = self.source.get(receiver.name.clone()).unwrap_or_default();
        let mut binding = None;
        for candidate in &self.bindings {
            if !work.step(cancel)? {
                return Ok(None);
            }
            if self.source.get(candidate.name.clone()) == Some(spelling)
                && (binding.replace(candidate).is_some()
                    || candidate.declaration.end > receiver.name.start)
            {
                return Ok(None);
            }
        }
        let Some(binding) = binding else {
            return Ok(None);
        };
        let mut allowed = HashSet::new();
        allowed
            .try_reserve(self.receivers.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        for other in &self.receivers {
            if !work.step(cancel)? {
                return Ok(None);
            }
            if binding.declaration.end <= other.name.start
                && self.source.get(other.name.clone()) == Some(spelling)
            {
                allowed.insert(other.name.clone());
            }
        }
        for token in &self.words {
            if !work.step(cancel)? {
                return Ok(None);
            }
            if self.source.get(token.range.clone()) != Some(spelling)
                || token.range == binding.name
                || self.nonbinding.contains(&token.range)
            {
                continue;
            }
            if !allowed.contains(&token.range) {
                return Ok(None);
            }
        }
        Ok(Some(binding))
    }
}

struct Binding {
    target: SymbolId,
    owner: SymbolId,
    span: SourceSpan,
    spelling: ContentHash,
    provenance: FactId,
}

/// Maps authenticated native receiver facts to current lexical Variable symbols.
pub(super) struct Targets {
    source: SourceRef,
    bindings: HashMap<u64, Binding>,
}

fn direct(child: &SyntaxFact, parent: &SyntaxFact) -> bool {
    child.parent() == Some(parent.local_id())
        && parent.depth().checked_add(1) == Some(child.depth())
        && child.span().file() == parent.span().file()
        && parent.span().start_byte() <= child.span().start_byte()
        && child.span().end_byte() <= parent.span().end_byte()
}

fn span(file: rootlight_ids::FileId, range: &Range<usize>) -> Option<SourceSpan> {
    SourceSpan::new(
        file,
        u64::try_from(range.start).ok()?,
        u64::try_from(range.end).ok()?,
    )
    .ok()
}

fn file_container(source: &SourceRef) -> [u8; 21] {
    let mut result = [0; 21];
    result[0] = 1;
    result[1..].copy_from_slice(source.span().file().as_bytes());
    result
}

fn variable_container(module: SymbolId) -> [u8; 54] {
    let mut result = [0; 54];
    result[0] = 2;
    result[1..21].copy_from_slice(module.as_bytes());
    result[21] = 3;
    // The existing census keys this ordinal by named owner, declaration label
    // and spelling. This complete module admits one binding for the spelling
    // and no intervening scopes; unrelated declarations use different counters.
    let scope = super::source_occurrence_identity(
        "rootlight.ecmascript-source-declaration/1",
        None,
        "javascript.variable.declaration",
        0,
    );
    result[22..].copy_from_slice(&scope);
    result
}

fn exact_identity(
    entity: &MaterializedEntity,
    source: &SourceRef,
    kind: EntityKind,
    name: &str,
    container: ContainerRef,
    container_identity: &[u8],
    build: BuildContextIdentity,
) -> bool {
    let claim = &entity.identity_claim;
    entity.record.repository == source.repository()
        && entity.record.language == "javascript"
        && entity.record.kind == kind
        && entity.record.canonical_name == name
        && entity.record.container == Some(container)
        && entity.direct_parent == container
        && claim.repository == source.repository()
        && claim.language == "javascript"
        && claim.kind == kind
        && claim.container == Some(container)
        && claim.container_identity.as_slice() == container_identity
        && claim.declared_identity == name
        && claim.signature_discriminator.is_empty()
        && claim.build_context_discriminator.as_slice() == build.digest().as_bytes().as_slice()
        && claim.symbol == entity.record.id
        && claim.derived_symbol() == entity.record.id
        && super::entity_identity_guard(
            source.repository(),
            "javascript",
            rootlight_ir::entity_kind_identity_label(kind),
            container_identity,
            name,
            &[],
            build.digest().as_bytes(),
            None,
        )
        .is_ok_and(|guard| guard == entity.identity_guard)
}

fn exact_projection(
    view: &NativeSourceView<'_>,
    physical: &[u8],
    work: &mut Work,
    cancel: &Cancellation,
) -> Result<bool, AdapterError> {
    let Some(encoding) = view.encoding() else {
        if view.as_bytes().len() != physical.len() {
            return Ok(false);
        }
        for (current, written) in view.as_bytes().chunks(64).zip(physical.chunks(64)) {
            if !work.step(cancel)? || current != written {
                return Ok(false);
            }
        }
        return Ok(true);
    };
    let expected_bom = match encoding {
        SourceTextEncoding::Utf16Le => [0xff, 0xfe],
        SourceTextEncoding::Utf16Be => [0xfe, 0xff],
    };
    if !physical.starts_with(&expected_bom) || !view.as_bytes().starts_with(b"\xef\xbb\xbf") {
        return Ok(false);
    }
    let mut at = 0usize;
    for scalar in view.as_str().chars() {
        if !work.step(cancel)? {
            return Ok(false);
        }
        let mut units = [0; 2];
        for unit in scalar.encode_utf16(&mut units) {
            let bytes = match encoding {
                SourceTextEncoding::Utf16Le => unit.to_le_bytes(),
                SourceTextEncoding::Utf16Be => unit.to_be_bytes(),
            };
            let Some(end) = at.checked_add(2) else {
                return Err(SinkError::AccountingOverflow.into());
            };
            if physical.get(at..end) != Some(bytes.as_slice()) {
                return Ok(false);
            }
            at = end;
        }
    }
    Ok(at == physical.len())
}

impl Targets {
    fn empty(source: &SourceRef) -> Self {
        Self {
            source: source.clone(),
            bindings: HashMap::new(),
        }
    }
    /// Authenticates the complete source grammar and raw/materialized owner joins.
    /// Invalid or exhausted partitions contain no bindings.
    ///
    /// # Errors
    /// Returns cancellation, allocation/accounting or coordinate-projection errors.
    #[expect(
        clippy::too_many_arguments,
        reason = "decoded grammar and physical frame require distinct authentication"
    )]
    pub(super) fn new(
        facts: &[SyntaxFact],
        entities: &HashMap<u64, MaterializedEntity>,
        source: &SourceRef,
        view: &NativeSourceView<'_>,
        physical_bytes: &[u8],
        module_name: &str,
        build_context: BuildContextIdentity,
        maximum: usize,
        cancel: &Cancellation,
    ) -> Result<Self, AdapterError> {
        let current = view.as_bytes();
        let physical = |span| view.physical_span(span);
        let mut result = Self {
            source: source.clone(),
            bindings: HashMap::new(),
        };
        let mut work = Work {
            remaining: maximum,
            exhausted: false,
        };
        let mut has_reference = false;
        for fact in facts {
            if !work.step(cancel)? {
                return Ok(result);
            }
            has_reference |= required(fact);
        }
        if !has_reference {
            return Ok(result);
        }
        if source.span().start_byte() != 0
            || u64::try_from(physical_bytes.len()).ok() != Some(source.span().end_byte())
        {
            return Ok(result);
        }
        let mut hash = blake3::Hasher::new();
        for chunk in physical_bytes.chunks(64) {
            if !work.step(cancel)? {
                return Ok(result);
            }
            hash.update(chunk);
        }
        if ContentHash::from_bytes(*hash.finalize().as_bytes()) != source.content_hash()
            || !exact_projection(view, physical_bytes, &mut work, cancel)?
        {
            return Ok(result);
        }
        let Some(program) = Program::new(current, &mut work, cancel)? else {
            return Ok(result);
        };
        let mut ids = HashMap::new();
        let mut by_shape = HashMap::new();
        let mut role_counts: HashMap<(SyntaxFactKind, SourceSpan), usize> = HashMap::new();
        role_counts
            .try_reserve(facts.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        ids.try_reserve(facts.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        by_shape
            .try_reserve(facts.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        let (mut root, mut module, mut clean) = (None, None, None);
        for fact in facts {
            if !work.step(cancel)? {
                return Ok(result);
            }
            if fact.span().file() != source.span().file()
                || !fact.syntax_kind().as_str().starts_with("javascript.")
                || (fact.kind() == SyntaxFactKind::Signature && !metadata(fact))
                || fact.kind() == SyntaxFactKind::ErrorRecovery
                || ids.insert(fact.local_id(), fact).is_some()
                || by_shape
                    .insert(
                        (fact.kind(), fact.syntax_kind().as_str(), fact.span()),
                        fact,
                    )
                    .is_some()
                || (fact.syntax_kind().as_str().starts_with(PREFIX)
                    && !metadata(fact)
                    && !required(fact))
            {
                return Ok(result);
            }
            let count = role_counts.entry((fact.kind(), fact.span())).or_default();
            *count = count.checked_add(1).ok_or(SinkError::AccountingOverflow)?;
            match (fact.kind(), fact.syntax_kind().as_str()) {
                (SyntaxFactKind::Root, "javascript.program.root") => {
                    if root.replace(fact).is_some() {
                        return Ok(result);
                    }
                }
                (SyntaxFactKind::Module, "javascript.file.module") => {
                    if module.replace(fact).is_some() {
                        return Ok(result);
                    }
                }
                (SyntaxFactKind::Signature, CLEAN) => {
                    if clean.replace(fact).is_some() {
                        return Ok(result);
                    }
                }
                (SyntaxFactKind::Root | SyntaxFactKind::Module | SyntaxFactKind::Scope, _) => {
                    return Ok(result);
                }
                _ => {}
            }
        }
        let (Some(root), Some(module), Some(clean)) = (root, module, clean) else {
            return Ok(result);
        };
        let root_physical = physical(root.span())?;
        let Some(native_root) = span(source.span().file(), &program.native_root) else {
            return Ok(result);
        };
        let envelope = SourceSpan::new(
            source.span().file(),
            0,
            u64::try_from(current.len()).map_err(|_| SinkError::AccountingOverflow)?,
        )
        .map_err(|_| super::provider_failure("javascript-const-root-span"))?;
        let prefix = SourceSpan::new(source.span().file(), 0, native_root.start_byte())
            .map_err(|_| super::provider_failure("javascript-const-root-prefix"))?;
        let physical_prefix = physical(prefix)?;
        let physical_native = physical(native_root)?;
        let expected_prefix: &[u8] = if program.native_root.start == 0 {
            b""
        } else {
            match view.encoding() {
                None => b"\xef\xbb\xbf",
                Some(SourceTextEncoding::Utf16Le) => b"\xff\xfe",
                Some(SourceTextEncoding::Utf16Be) => b"\xfe\xff",
            }
        };
        if root.parent().is_some()
            || root.depth() != 0
            || root.span() != envelope
            || root_physical != source.span()
            || physical_prefix.start_byte() != 0
            || usize::try_from(physical_prefix.end_byte()).ok() != Some(expected_prefix.len())
            || physical_bytes.get(..expected_prefix.len()) != Some(expected_prefix)
            || physical_native.start_byte() != physical_prefix.end_byte()
            || physical_native.end_byte() != source.span().end_byte()
            || module.span() != envelope
            || !direct(module, root)
            || clean.span() != envelope
            || !direct(clean, module)
        {
            return Ok(result);
        }
        let Some(module_entity) = entities.get(&module.local_id()).filter(|entity| {
            entity.record.kind == EntityKind::Module
                && entity.record.language == "javascript"
                && entity.record.repository == source.repository()
                && entity.record.generation == source.generation()
                && entity.record.evidence.source.as_ref()
                    == Some(&super::source_for_span(source, root_physical))
                && entity.direct_parent == ContainerRef::File(source.span().file())
                && entity.record.container == Some(entity.direct_parent)
                && entity.record.canonical_name == module_name
                && entity.record.qualified_name == module_name
                && entity.definition_local_id.is_none()
                && entity.signature_evidence.is_none()
                && entity.signature_span.is_none()
                && exact_identity(
                    entity,
                    source,
                    EntityKind::Module,
                    module_name,
                    ContainerRef::File(source.span().file()),
                    &file_container(source),
                    build_context,
                )
        }) else {
            return Ok(result);
        };
        let mut symbols = HashSet::new();
        symbols
            .try_reserve(entities.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        for entity in entities.values() {
            if !work.step(cancel)? || !symbols.insert(entity.record.id) {
                return Ok(result);
            }
        }
        let mut written = Vec::new();
        written
            .try_reserve(program.receivers.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        let mut expected = HashSet::new();
        let expected_count = program
            .receivers
            .len()
            .checked_mul(5)
            .and_then(|count| count.checked_add(program.nonbinding.len()))
            .and_then(|count| count.checked_add(1))
            .ok_or(SinkError::AccountingOverflow)?;
        expected
            .try_reserve(expected_count)
            .map_err(|_| SinkError::AllocationFailed)?;
        expected.insert((SyntaxFactKind::Signature, CLEAN, root.span()));
        for range in &program.nonbinding {
            if !work.step(cancel)? {
                return Ok(result);
            }
            let Some(range) = span(source.span().file(), range) else {
                return Ok(result);
            };
            expected.insert((SyntaxFactKind::Signature, NONBINDING, range));
        }
        for receiver in &program.receivers {
            if !work.step(cancel)? {
                return Ok(result);
            }
            let Some(binding) = program.binding(receiver, &mut work, cancel)? else {
                continue;
            };
            for (kind, label, range) in [
                (SyntaxFactKind::Occurrence, REFERENCE, receiver.name.clone()),
                (
                    SyntaxFactKind::Signature,
                    BINDING,
                    binding.declaration.clone(),
                ),
                (
                    SyntaxFactKind::Signature,
                    TARGET,
                    binding.name.start..receiver.name.end,
                ),
                (SyntaxFactKind::Signature, CALL, receiver.call.clone()),
                (SyntaxFactKind::Signature, OWNER, receiver.owner.clone()),
            ] {
                let Some(range) = span(source.span().file(), &range) else {
                    return Ok(result);
                };
                expected.insert((kind, label, range));
            }
            written.push((receiver, binding));
        }
        if work.exhausted {
            return Ok(result);
        }
        for row in &expected {
            if !work.step(cancel)? || !by_shape.contains_key(row) {
                return Ok(result);
            }
        }
        for fact in facts {
            if !work.step(cancel)?
                || ((metadata(fact) || required(fact))
                    && !expected.contains(&(fact.kind(), fact.syntax_kind().as_str(), fact.span())))
            {
                return Ok(result);
            }
        }
        let lookup = |kind, label, range: &Range<usize>| {
            span(source.span().file(), range)
                .and_then(|span| by_shape.get(&(kind, label, span)).copied())
        };
        for range in &program.nonbinding {
            if !work.step(cancel)? {
                return Ok(Self {
                    source: source.clone(),
                    bindings: HashMap::new(),
                });
            }
            let Some(marker) = lookup(SyntaxFactKind::Signature, NONBINDING, range) else {
                return Ok(result);
            };
            if !direct(marker, module) {
                return Ok(result);
            }
        }
        for fact in facts {
            if !work.step(cancel)? {
                return Ok(Self {
                    source: source.clone(),
                    bindings: HashMap::new(),
                });
            }
            if fact.syntax_kind().as_str() == NONBINDING {
                let range = usize::try_from(fact.span().start_byte())
                    .ok()
                    .zip(usize::try_from(fact.span().end_byte()).ok())
                    .map(|(start, end)| start..end);
                if range.is_none_or(|range| !program.nonbinding.contains(&range)) {
                    return Ok(result);
                }
            }
        }
        for (receiver, binding) in written {
            if !work.step(cancel)? {
                return Ok(Self {
                    source: source.clone(),
                    bindings: HashMap::new(),
                });
            }
            let (
                Some(reference),
                Some(declaration),
                Some(definition),
                Some(binding_marker),
                Some(target),
                Some(call),
                Some(owner),
            ) = (
                lookup(SyntaxFactKind::Occurrence, REFERENCE, &receiver.name),
                lookup(
                    SyntaxFactKind::Declaration,
                    "javascript.variable.declaration",
                    &binding.declaration,
                ),
                lookup(
                    SyntaxFactKind::Occurrence,
                    "javascript.identifier.definition",
                    &binding.name,
                ),
                lookup(SyntaxFactKind::Signature, BINDING, &binding.declaration),
                lookup(
                    SyntaxFactKind::Signature,
                    TARGET,
                    &(binding.name.start..receiver.name.end),
                ),
                lookup(SyntaxFactKind::Signature, CALL, &receiver.call),
                lookup(SyntaxFactKind::Signature, OWNER, &receiver.owner),
            )
            else {
                return Ok(Self::empty(source));
            };
            if role_counts.get(&(SyntaxFactKind::Occurrence, reference.span())) != Some(&1)
                || role_counts.get(&(SyntaxFactKind::Declaration, declaration.span())) != Some(&1)
                || role_counts.get(&(SyntaxFactKind::Occurrence, definition.span())) != Some(&1)
                || !direct(reference, module)
                || !direct(declaration, module)
                || !direct(definition, declaration)
                || !direct(binding_marker, declaration)
                || !direct(target, module)
                || !direct(call, module)
                || !direct(owner, module)
            {
                return Ok(Self::empty(source));
            }
            let Some(variable) = entities.get(&declaration.local_id()) else {
                return Ok(Self::empty(source));
            };
            let target_span = physical(declaration.span())?;
            let expected_name = current.get(binding.name.clone()).unwrap_or_default();
            if variable.record.kind != EntityKind::Variable
                || variable.record.language != "javascript"
                || variable.record.repository != source.repository()
                || variable.record.generation != source.generation()
                || variable.definition_local_id != Some(definition.local_id())
                || variable.direct_parent != ContainerRef::Entity(module_entity.record.id)
                || variable.record.container != Some(variable.direct_parent)
                || variable.record.evidence.source.as_ref()
                    != Some(&super::source_for_span(source, target_span))
                || variable.record.provenance != module_entity.record.provenance
                || variable.record.canonical_name.as_bytes() != expected_name
                || variable.signature_evidence.is_some()
                || variable.signature_span.is_some()
                || !exact_identity(
                    variable,
                    source,
                    EntityKind::Variable,
                    &variable.record.canonical_name,
                    ContainerRef::Entity(module_entity.record.id),
                    &variable_container(module_entity.record.id),
                    build_context,
                )
            {
                return Ok(Self::empty(source));
            }
            let mut spelling = blake3::Hasher::new();
            for chunk in expected_name.chunks(64) {
                if !work.step(cancel)? {
                    return Ok(Self {
                        source: source.clone(),
                        bindings: HashMap::new(),
                    });
                }
                spelling.update(chunk);
            }
            result
                .bindings
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            result.bindings.insert(
                reference.local_id(),
                Binding {
                    target: variable.record.id,
                    owner: module_entity.record.id,
                    span: physical(reference.span())?,
                    spelling: ContentHash::from_bytes(*spelling.finalize().as_bytes()),
                    provenance: variable.record.provenance,
                },
            );
        }
        if work.exhausted {
            result.bindings.clear();
        }
        Ok(result)
    }

    /// Resolves only the unchanged native unresolved Reference in this source frame.
    ///
    /// # Errors
    /// Returns cancellation without mutating the supplied occurrence.
    pub(super) fn resolve(
        &self,
        fact: &SyntaxFact,
        physical: SourceSpan,
        occurrence: &OccurrenceRecord,
        cancel: &Cancellation,
    ) -> Result<Option<SymbolId>, AdapterError> {
        cancel.check()?;
        let Some(binding) = self.bindings.get(&fact.local_id()) else {
            return Ok(None);
        };
        Ok((required(fact) && binding.span == physical && occurrence.source == super::source_for_span(&self.source, physical)
            && occurrence.repository == self.source.repository() && occurrence.generation == self.source.generation() && occurrence.file == physical.file()
            && occurrence.role == OccurrenceRole::Reference && occurrence.syntax_kind == REFERENCE
            && occurrence.syntactic_text_hash == binding.spelling && occurrence.enclosing == Some(binding.owner)
            && occurrence.provenance == binding.provenance && occurrence.evidence.source.as_ref() == Some(&occurrence.source)
            && matches!(occurrence.target, OccurrenceTarget::Unresolved { text_hash } if text_hash == binding.spelling)).then_some(binding.target))
    }
}

#[cfg(test)]
mod tests;
