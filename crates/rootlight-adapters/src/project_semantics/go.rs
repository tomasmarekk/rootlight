//! Project-local Go function bindings from paired native sites and current source.
//! Package membership includes the authored directory; names alone cannot repair
//! an absent site, declaration, Definition or complete function certificate.

use super::*;

pub(super) struct Proof {
    pub(super) target: SymbolId,
    pub(super) owner_local: u64,
    pub(super) call: bool,
    pub(super) derivation: Vec<FactRef>,
}

/// These required carriers survive the project's optional syntax selection.
pub(super) fn mandatory(fact: &SyntaxFact) -> bool {
    site(fact).is_some() || metadata(fact)
}

/// Site certificates describe bindings, not public function signatures.
pub(super) fn metadata(fact: &SyntaxFact) -> bool {
    fact.kind() == SyntaxFactKind::Signature
        && matches!(
            fact.syntax_kind().as_str(),
            "go.package_function_context.signature" | "go.function_value_context.signature"
        )
}

/// Restrict span-indexed project proof reuse to the three exact carrier labels.
pub(super) fn occurrence(label: &str) -> bool {
    matches!(
        label,
        "go.direct_file_function.reference"
            | "go.package_function_candidate.reference"
            | "go.function_value.reference"
    )
}

fn site(fact: &SyntaxFact) -> Option<bool> {
    if fact.kind() != SyntaxFactKind::Occurrence {
        return None;
    }
    match fact.syntax_kind().as_str() {
        "go.direct_file_function.reference" | "go.package_function_candidate.reference" => {
            Some(true)
        }
        "go.function_value.reference" => Some(false),
        _ => None,
    }
}

struct Function<'a> {
    local: u64,
    symbol: SymbolId,
    span: SourceSpan,
    name: &'a [u8],
    body: usize,
}

struct File<'a> {
    source: SourceRef,
    directory: &'a str,
    package: &'a [u8],
    facts: BTreeMap<u64, &'a SyntaxFact>,
    tokens: Vec<Token<'a>>,
    names: BTreeMap<ContentHash, usize>,
    functions: BTreeMap<u64, Function<'a>>,
    references: BTreeMap<SourceSpan, Option<&'a SyntaxFact>>,
    signatures: BTreeMap<SourceSpan, Option<&'a SyntaxFact>>,
}

/// Bind only the finite ordinary function subset; every absent proof stays fenced.
/// Cancellation and allocation errors remain typed and abort the request.
pub(super) fn resolve(
    builder: &ProjectFactsBuilder<'_, '_, '_>,
) -> Result<BTreeMap<SourceSpan, Proof>, AdapterError> {
    let mut files = BTreeMap::new();
    let mut unavailable_directories = BTreeSet::new();
    for parsed in &builder.parsed {
        builder.cancellation.check()?;
        let file = parsed.input.source().source_ref().span().file();
        let path = &builder.path_by_file[&file];
        let directory = path.rsplit_once('/').map_or("", |(head, _)| head);
        if let Some(syntax) = file_syntax(builder, parsed, directory)? {
            files.insert(file, syntax);
        } else {
            unavailable_directories.insert(directory);
        }
    }
    let mut output = BTreeMap::new();
    for occurrence in &builder.occurrences {
        builder.cancellation.check()?;
        if occurrence.role != OccurrenceRole::Reference {
            continue;
        }
        let Some(file) = files.get(&occurrence.file) else {
            continue;
        };
        if unavailable_directories.contains(file.directory)
            || occurrence.source
                != source_for_span(
                    builder.input_for_file(occurrence.file)?,
                    occurrence.source.span(),
                )
        {
            continue;
        }
        let Some(fact) = file
            .references
            .get(&occurrence.source.span())
            .copied()
            .flatten()
        else {
            continue;
        };
        let Some(call) = site(fact) else { continue };
        if fact.syntax_kind().as_str() != occurrence.syntax_kind {
            continue;
        }
        let Some(context) = file.signatures.get(&fact.span()).copied().flatten() else {
            continue;
        };
        if context.syntax_kind().as_str()
            != if call {
                "go.package_function_context.signature"
            } else {
                "go.function_value_context.signature"
            }
            || context.parent() != fact.parent()
            || context.depth() != fact.depth()
            || context.span() != fact.span()
        {
            continue;
        }
        let Some(owner) = owner(file, fact, builder.cancellation)? else {
            continue;
        };
        if fact.span().start_byte() < owner.body as u64 {
            continue;
        }
        let position = file
            .tokens
            .partition_point(|token| (token.start as u64) < fact.span().start_byte());
        let Some(token) = file.tokens.get(position).copied().filter(|token| {
            token.start as u64 == fact.span().start_byte()
                && token.end as u64 == fact.span().end_byte()
                && token.identifier
        }) else {
            continue;
        };
        if !same(
            token.bytes,
            occurrence.name.as_bytes(),
            builder.cancellation,
        )? || !site_position(&file.tokens, position, call)
            || !unshadowed(file, owner, token.bytes, builder.cancellation)?
        {
            continue;
        }
        let key = name_key(token.bytes, builder.cancellation)?;
        let mut count = 0usize;
        let mut target = None;
        for sibling in files.values() {
            builder.cancellation.check()?;
            if sibling.directory != file.directory
                || !same(sibling.package, file.package, builder.cancellation)?
            {
                continue;
            }
            count = count
                .checked_add(sibling.names.get(&key).copied().unwrap_or(0))
                .ok_or(SinkError::AccountingOverflow)?;
            for function in sibling.functions.values() {
                builder.cancellation.check()?;
                if same(function.name, token.bytes, builder.cancellation)?
                    && target.replace((sibling, function)).is_some()
                {
                    count = count.saturating_add(1);
                }
            }
        }
        let Some((target_file, target)) = target.filter(|_| count == 1) else {
            continue;
        };
        let mut derivation = BTreeSet::from([
            FactRef::File(occurrence.file),
            FactRef::File(target.span.file()),
            FactRef::Fact(builder.provenance_for_file(occurrence.file)?),
            FactRef::Fact(builder.provenance_for_file(target.span.file())?),
            FactRef::Entity(owner.symbol),
            FactRef::Entity(target.symbol),
        ]);
        if target_file.source.repository() != file.source.repository()
            || target_file.source.generation() != file.source.generation()
            || derivation.len() > builder.request.limits().ir().max_nested_items_per_record
        {
            continue;
        }
        output.insert(
            fact.span(),
            Proof {
                target: target.symbol,
                owner_local: owner.local,
                call,
                derivation: std::mem::take(&mut derivation).into_iter().collect(),
            },
        );
    }
    Ok(output)
}

fn unique<K: Ord, V>(map: &mut BTreeMap<K, Option<V>>, key: K, value: V) {
    map.entry(key)
        .and_modify(|entry| *entry = None)
        .or_insert(Some(value));
}

fn file_syntax<'a>(
    builder: &'a ProjectFactsBuilder<'_, '_, '_>,
    parsed: &'a ParsedInput<'_, '_>,
    directory: &'a str,
) -> Result<Option<File<'a>>, AdapterError> {
    let cancel = builder.cancellation;
    let source = parsed.input.source().source_ref();
    let bytes = parsed.input.source().bytes();
    if parsed.parse_status != CoverageStatus::Complete
        || parsed.input.encoding().as_str() != "utf-8"
    {
        return Ok(None);
    }
    let mut facts = BTreeMap::new();
    let mut declarations = BTreeMap::new();
    let mut definitions = BTreeMap::new();
    let mut definition_spans = BTreeMap::new();
    let mut references = BTreeMap::new();
    let mut signatures = BTreeMap::new();
    let mut root = None;
    let mut module = None;
    for fact in &parsed.facts {
        cancel.check()?;
        if facts.insert(fact.local_id(), fact).is_some()
            || fact.span().file() != source.span().file()
            || !contains_span(source.span(), fact.span())
            || fact.kind() == SyntaxFactKind::ErrorRecovery
        {
            return Ok(None);
        }
        match fact.kind() {
            SyntaxFactKind::Root => {
                if root.replace(fact).is_some()
                    || fact.syntax_kind().as_str() != "go.file.root"
                    || fact.parent().is_some()
                    || fact.depth() != 0
                    || fact.span() != source.span()
                {
                    return Ok(None);
                }
            }
            SyntaxFactKind::Module if fact.syntax_kind().as_str() == "go.package.module" => {
                if module.replace(fact).is_some() {
                    return Ok(None);
                }
            }
            SyntaxFactKind::Declaration => unique(&mut declarations, fact.span(), fact),
            SyntaxFactKind::Signature => unique(&mut signatures, fact.span(), fact),
            SyntaxFactKind::Occurrence if fact.syntax_kind().as_str().ends_with(".definition") => {
                unique(&mut definitions, fact.parent(), fact);
                unique(&mut definition_spans, fact.span(), fact);
            }
            SyntaxFactKind::Occurrence if fact.syntax_kind().as_str().ends_with(".reference") => {
                unique(&mut references, fact.span(), fact);
            }
            _ => {}
        }
    }
    let (Some(root), Some(module)) = (root, module) else {
        return Ok(None);
    };
    if module.parent() != Some(root.local_id()) || !contains_span(root.span(), module.span()) {
        return Ok(None);
    }
    let Some(package_definition) = definitions.get(&Some(module.local_id())).copied().flatten()
    else {
        return Ok(None);
    };
    if package_definition.syntax_kind().as_str() != "go.package_identifier.definition"
        || definition_spans
            .get(&package_definition.span())
            .copied()
            .flatten()
            .map(SyntaxFact::local_id)
            != Some(package_definition.local_id())
    {
        return Ok(None);
    }
    let Some(tokens) = lex(bytes, cancel)? else {
        return Ok(None);
    };
    let Some(package) = tokens.get(1).filter(|token| token.identifier) else {
        return Ok(None);
    };
    if tokens.first().is_none_or(|token| token.bytes != b"package")
        || package.start as u64 != package_definition.span().start_byte()
        || package.end as u64 != package_definition.span().end_byte()
        || module.span().start_byte() != tokens[0].start as u64
        || module.span().end_byte() != package.end as u64
    {
        return Ok(None);
    }
    // Every current top-level identifier contributes before materialization.
    // Parameter/header/import occurrences may conservatively refuse a spelling;
    // missing raw Definitions can never make a competing namespace name disappear.
    let mut names = BTreeMap::<ContentHash, usize>::new();
    let mut braces = 0usize;
    for (index, token) in tokens.iter().enumerate().skip(2) {
        check_periodically(index, cancel)?;
        if token.bytes == b"}" {
            let Some(next) = braces.checked_sub(1) else {
                return Ok(None);
            };
            braces = next;
        }
        if braces == 0
            && token.bytes == b"."
            && tokens
                .get(index + 1)
                .and_then(|next| next.bytes.first())
                .is_some_and(|byte| matches!(*byte, b'"' | b'`'))
        {
            return Ok(None);
        }
        if braces == 0 && token.identifier {
            let count = names.entry(name_key(token.bytes, cancel)?).or_default();
            *count = count.checked_add(1).ok_or(SinkError::AccountingOverflow)?;
        }
        if token.bytes == b"{" {
            braces = braces.checked_add(1).ok_or(SinkError::AccountingOverflow)?;
        }
    }
    if braces != 0 {
        return Ok(None);
    }
    let mut functions = BTreeMap::new();
    for draft in &builder.declarations {
        cancel.check()?;
        if draft.file != source.span().file()
            || draft.kind != EntityKind::Function
            || draft.synthetic
            || draft.parent_declaration.is_some()
            || draft.declaring_type.is_some()
        {
            continue;
        }
        let Some(declaration) = facts.get(&draft.local_id).copied() else {
            continue;
        };
        let Some(definition) = definitions.get(&Some(draft.local_id)).copied().flatten() else {
            continue;
        };
        let Some(context) = signatures.get(&declaration.span()).copied().flatten() else {
            continue;
        };
        if declaration.syntax_kind().as_str() != "go.function.declaration"
            || declaration.parent() != Some(root.local_id())
            || declarations
                .get(&declaration.span())
                .copied()
                .flatten()
                .map(SyntaxFact::local_id)
                != Some(draft.local_id)
            || definition.syntax_kind().as_str() != "go.identifier.definition"
            || definition_spans
                .get(&definition.span())
                .copied()
                .flatten()
                .map(SyntaxFact::local_id)
                != Some(definition.local_id())
            || context.parent() != Some(draft.local_id)
            || !matches!(
                context.syntax_kind().as_str(),
                "go.call_formal_context.signature" | "go.composite_formal_context.signature"
            )
            || draft.span != declaration.span()
            || draft.source != source_for_span(parsed.input, declaration.span())
        {
            continue;
        }
        let Some(symbol) = builder
            .symbol_by_declaration
            .get(&(draft.file, draft.local_id))
            .copied()
        else {
            continue;
        };
        let Some((name, body)) =
            function_header(&tokens, declaration.span(), definition.span(), cancel)?
        else {
            continue;
        };
        if !same(name, draft.name.as_bytes(), cancel)?
            || names.get(&name_key(name, cancel)?) != Some(&1)
        {
            continue;
        }
        functions.insert(
            draft.local_id,
            Function {
                local: draft.local_id,
                symbol,
                span: draft.span,
                name,
                body,
            },
        );
    }
    Ok(Some(File {
        source: source.clone(),
        directory,
        package: package.bytes,
        facts,
        tokens,
        names,
        functions,
        references,
        signatures,
    }))
}

fn function_header<'a>(
    tokens: &[Token<'a>],
    span: SourceSpan,
    definition: SourceSpan,
    cancel: &Cancellation,
) -> Result<Option<(&'a [u8], usize)>, AdapterError> {
    let first = tokens.partition_point(|token| (token.start as u64) < span.start_byte());
    let end = tokens.partition_point(|token| (token.end as u64) <= span.end_byte());
    let Some(row) = tokens.get(first..end) else {
        return Ok(None);
    };
    let [keyword, name, opening, ..] = row else {
        return Ok(None);
    };
    if keyword.start as u64 != span.start_byte()
        || keyword.bytes != b"func"
        || !name.identifier
        || matches!(name.bytes, b"_" | b"init")
        || opening.bytes != b"("
        || name.start as u64 != definition.start_byte()
        || name.end as u64 != definition.end_byte()
        || row
            .last()
            .is_none_or(|last| last.end as u64 != span.end_byte() || last.bytes != b"}")
    {
        return Ok(None);
    }
    let mut depth = 0usize;
    let mut braces = 0usize;
    let mut body = None;
    for (index, token) in row[2..].iter().enumerate() {
        check_periodically(index, cancel)?;
        if body.is_none() {
            if token.bytes == b"(" {
                depth = depth.checked_add(1).ok_or(SinkError::AccountingOverflow)?;
            }
            if token.bytes == b")" {
                let Some(next) = depth.checked_sub(1) else {
                    return Ok(None);
                };
                depth = next;
            }
            if depth == 0 && matches!(token.bytes, b"struct" | b"interface") {
                return Ok(None);
            }
            if depth == 0 && token.bytes == b"{" {
                body = Some(token.end);
                braces = 1;
            }
        } else {
            if token.bytes == b"{" {
                braces = braces.checked_add(1).ok_or(SinkError::AccountingOverflow)?;
            }
            if token.bytes == b"}" {
                let Some(next) = braces.checked_sub(1) else {
                    return Ok(None);
                };
                braces = next;
                if braces == 0 {
                    return Ok((token.end as u64 == span.end_byte())
                        .then(|| (name.bytes, body.expect("body entry set the matching brace"))));
                }
            }
        }
    }
    Ok(None)
}

fn owner<'a>(
    file: &'a File<'_>,
    fact: &SyntaxFact,
    cancel: &Cancellation,
) -> Result<Option<&'a Function<'a>>, AdapterError> {
    let mut parent = fact.parent();
    for _ in 0..file.facts.len() {
        cancel.check()?;
        let Some(raw) = parent.and_then(|id| file.facts.get(&id)).copied() else {
            return Ok(None);
        };
        if raw.kind() == SyntaxFactKind::Declaration
            && raw.syntax_kind().as_str() == "go.function.declaration"
        {
            return Ok(file
                .functions
                .get(&raw.local_id())
                .filter(|owner| contains_span(owner.span, fact.span())));
        }
        if raw.kind() == SyntaxFactKind::Declaration
            && !matches!(
                raw.syntax_kind().as_str(),
                "go.variable.declaration" | "go.constant.declaration"
            )
        {
            return Ok(None);
        }
        if !contains_span(raw.span(), fact.span()) {
            return Ok(None);
        }
        parent = raw.parent();
    }
    Ok(None)
}

fn site_position(tokens: &[Token<'_>], position: usize, call: bool) -> bool {
    let Some(previous) = position.checked_sub(1).and_then(|index| tokens.get(index)) else {
        return false;
    };
    let Some(next) = tokens.get(position + 1) else {
        return false;
    };
    if matches!(previous.bytes, b"." | b"::") {
        return false;
    }
    if call {
        next.bytes == b"("
    } else {
        matches!(previous.bytes, b"(" | b",") && matches!(next.bytes, b"," | b")")
    }
}

fn unshadowed(
    file: &File<'_>,
    owner: &Function<'_>,
    name: &[u8],
    cancel: &Cancellation,
) -> Result<bool, AdapterError> {
    for (index, token) in file.tokens.iter().enumerate() {
        check_periodically(index, cancel)?;
        if (token.start as u64) < owner.span.start_byte()
            || (token.end as u64) > owner.span.end_byte()
        {
            continue;
        }
        if token.start >= owner.body && token.bytes == b"func" {
            return Ok(false);
        }
        if !token.identifier || !same(token.bytes, name, cancel)? {
            continue;
        }
        // The owner's written name is a namespace declaration, not a body use.
        if token.start < owner.body {
            if index > 0 && file.tokens[index - 1].bytes == b"func" {
                continue;
            }
            return Ok(false);
        }
        if !site_position(&file.tokens, index, true) && !site_position(&file.tokens, index, false) {
            return Ok(false);
        }
    }
    Ok(true)
}

#[derive(Clone, Copy)]
struct Token<'a> {
    bytes: &'a [u8],
    start: usize,
    end: usize,
    identifier: bool,
}

fn lex<'a>(bytes: &'a [u8], cancel: &Cancellation) -> Result<Option<Vec<Token<'a>>>, AdapterError> {
    let mut tokens = Vec::new();
    let mut index = usize::from(bytes.starts_with(b"\xef\xbb\xbf")) * 3;
    while index < bytes.len() {
        check_periodically(index, cancel)?;
        let start = index;
        let byte = bytes[index];
        if byte.is_ascii_whitespace() {
            index += 1;
            continue;
        }
        if bytes.get(index..index + 2) == Some(b"//") {
            while index < bytes.len() && bytes[index] != b'\n' {
                check_periodically(index, cancel)?;
                index += 1;
            }
            continue;
        }
        if bytes.get(index..index + 2) == Some(b"/*") {
            index += 2;
            while index + 1 < bytes.len() && bytes.get(index..index + 2) != Some(b"*/") {
                check_periodically(index, cancel)?;
                index += 1;
            }
            if index + 1 >= bytes.len() {
                return Ok(None);
            }
            index += 2;
            continue;
        }
        let identifier = byte.is_ascii_alphabetic() || byte == b'_';
        if identifier {
            index += 1;
            while index < bytes.len()
                && (bytes[index].is_ascii_alphanumeric() || bytes[index] == b'_')
            {
                check_periodically(index, cancel)?;
                index += 1;
            }
        } else if matches!(byte, b'"' | b'\'' | b'`') {
            index += 1;
            let mut closed = false;
            while index < bytes.len() {
                check_periodically(index, cancel)?;
                if bytes[index] == byte {
                    index += 1;
                    closed = true;
                    break;
                }
                if byte != b'`' && bytes[index] == b'\\' {
                    index = index.saturating_add(2);
                } else {
                    index += 1;
                }
            }
            if !closed {
                return Ok(None);
            }
        } else {
            if !byte.is_ascii() || byte == 0 {
                return Ok(None);
            }
            index += 1;
        }
        tokens
            .try_reserve(1)
            .map_err(|_| SinkError::AllocationFailed)?;
        tokens.push(Token {
            bytes: &bytes[start..index],
            start,
            end: index,
            identifier,
        });
    }
    Ok(Some(tokens))
}

fn name_key(bytes: &[u8], cancel: &Cancellation) -> Result<ContentHash, AdapterError> {
    let mut hash = blake3::Hasher::new();
    for chunk in bytes.chunks(64) {
        cancel.check()?;
        hash.update(chunk);
    }
    Ok(ContentHash::from_bytes(*hash.finalize().as_bytes()))
}

fn same(left: &[u8], right: &[u8], cancel: &Cancellation) -> Result<bool, AdapterError> {
    cancel.check()?;
    if left.len() != right.len() {
        return Ok(false);
    }
    for (left, right) in left.chunks(64).zip(right.chunks(64)) {
        cancel.check()?;
        if left != right {
            return Ok(false);
        }
    }
    Ok(true)
}
