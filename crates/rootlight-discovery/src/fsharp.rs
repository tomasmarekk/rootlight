//! Bounded declaration evidence for the `.fs` suffix shared by F# and Forth.
//! Explicit scopes or compound implicit-module evidence change routing only;
//! literal text, comments, and mixed Forth definitions provide no such hint.
//! A separate lexical/conflict guard permits native proof without a content hint.

use std::ops::Range;

pub(super) fn native_probe_allowed(
    source: &[u8],
    truncated: bool,
    original_budget_exhausted: bool,
) -> bool {
    if source.len() > super::MAX_CLASSIFICATION_BYTES {
        return false;
    }
    let Ok(source) = std::str::from_utf8(source) else {
        return false;
    };
    let source = source.strip_prefix('\u{feff}').unwrap_or(source);
    // A probe may lack declaration evidence, but must not hide conflicting text
    // inside a quoted/comment context whose closure lies outside the sample.
    let Some(masked) = mask_quoted_and_comment_contents(source.as_bytes(), false) else {
        return false;
    };
    let Ok(source) = std::str::from_utf8(&masked) else {
        return false;
    };
    // An unowned colon remains ambiguous even at true EOF. Only the existing
    // finite annotation proofs may remove that veto; eligibility is not routing.
    !has_forth_definition(source, true, truncated && original_budget_exhausted)
        && !has_forth_product_terminator(source)
}

pub(super) fn has_content_hint(
    source: &[u8],
    truncated: bool,
    original_budget_exhausted: bool,
) -> bool {
    if source.len() > super::MAX_CLASSIFICATION_BYTES {
        return false;
    }
    // A validated UTF-16 prefix can exhaust the original byte budget before
    // its decoded view reaches this length; the admitting caller owns that fact.
    let exhausted_sample = truncated && original_budget_exhausted;
    let source = match std::str::from_utf8(source) {
        Ok(source) => source,
        Err(error) if truncated && error.error_len().is_none() => {
            // Only a scalar split by the sample boundary may be ignored.
            let Ok(source) = std::str::from_utf8(&source[..error.valid_up_to()]) else {
                return false;
            };
            source
        }
        Err(_) => return false,
    };
    let source = source.strip_prefix('\u{feff}').unwrap_or(source);
    let raw_source = source;
    let Some(masked) = mask_quoted_and_comment_contents(source.as_bytes(), truncated) else {
        return false;
    };
    let Ok(source) = std::str::from_utf8(&masked) else {
        return false;
    };
    // The partial final line still supplies conflicting evidence, but cannot
    // supply a new declaration anchor whose end lies outside this sample.
    let evidence = if truncated {
        source.rfind('\n').map_or("", |end| &source[..end + 1])
    } else {
        source
    };
    let mut lines = evidence
        .lines()
        .zip(raw_source.lines())
        .filter(|(line, _)| !line.trim().is_empty())
        .peekable();
    let mut has_prologue = false;
    let mut implicit_prefix_indent = None;
    loop {
        let Some((line, raw_line)) = lines.next() else {
            return false;
        };
        if implicit_prefix_indent.is_some_and(|indent| space_indentation(line) != Some(indent)) {
            return false;
        }
        if implicit_prefix_indent.is_none() && module_or_namespace(line.trim()) {
            break;
        }
        if implicit_prefix_indent.is_none() && line.split_whitespace().next() == Some("#if") {
            if !complete_conditional_scope(line, raw_line, &mut lines) {
                return false;
            }
            break;
        }
        if implicit_declaration(line, lines.peek().map(|(line, _)| *line), has_prologue) {
            return !has_forth_definition(source, truncated, exhausted_sample)
                && !has_forth_product_terminator(source);
        }
        if implicit_prefix_indent.is_none() && finite_prologue_line(line.trim(), raw_line.trim()) {
            has_prologue = true;
            continue;
        }
        if has_prologue || !finite_implicit_declaration_prefix(line) {
            return false;
        }
        // Weak declarations cannot establish language by repetition or borrow
        // a nested body's annotation. Only a later independent cue can admit them.
        let Some(indent) = space_indentation(line) else {
            return false;
        };
        implicit_prefix_indent = Some(indent);
    }
    if has_forth_definition(source, truncated, exhausted_sample)
        || has_forth_product_terminator(source)
    {
        return false;
    }
    while let Some((line, _)) = lines.next() {
        if declaration(line, lines.peek().map(|(line, _)| *line)) {
            return true;
        }
    }
    false
}

fn complete_conditional_scope<'a>(
    masked: &str,
    raw: &str,
    lines: &mut impl Iterator<Item = (&'a str, &'a str)>,
) -> bool {
    let Some(opening) = authored_line(masked, raw) else {
        return false;
    };
    let mut words = opening.split_whitespace();
    if words.next() != Some("#if")
        || !words
            .next()
            .is_some_and(|symbol| identifier(symbol) && !matches!(symbol, "true" | "false"))
        || words.next().is_some()
    {
        return false;
    }
    let (Some(first), Some(separator), Some(alternate), Some(closing)) =
        (lines.next(), lines.next(), lines.next(), lines.next())
    else {
        return false;
    };
    let (Some(first), Some(separator), Some(alternate), Some(closing)) = (
        authored_line(first.0, first.1),
        authored_line(separator.0, separator.1),
        authored_line(alternate.0, alternate.1),
        authored_line(closing.0, closing.1),
    ) else {
        return false;
    };
    // Both complete authored alternatives establish source language only.
    // No compilation symbol is evaluated or namespace membership selected.
    module_or_namespace(first)
        && separator == "#else"
        && module_or_namespace(alternate)
        && closing == "#endif"
}

fn authored_line<'a>(masked: &str, raw: &'a str) -> Option<&'a str> {
    let start = masked.len().checked_sub(masked.trim_start().len())?;
    let end = masked.trim_end().len();
    let visible = masked.get(start..end)?;
    let authored = raw.get(start..end)?;
    // Leading/trailing complete comments may be trivia; removed bytes inside
    // the header or directive cannot manufacture a source-authored token.
    (authored == visible).then_some(authored)
}

fn finite_prologue_line(masked: &str, raw: &str) -> bool {
    // Masking cannot prove directive arguments or simple path spelling.
    // Raw and masked lines share byte/newline boundaries and must agree on
    // the visible form; none of these lines is a declaration anchor itself.
    if masked.starts_with("open") {
        return raw
            .strip_prefix("open")
            .filter(|path| path.starts_with(char::is_whitespace))
            .is_some_and(|path| path.trim().split('.').all(type_path_identifier));
    }
    if masked.starts_with("[<") {
        return raw
            .strip_prefix("[<")
            .and_then(|attribute| attribute.strip_suffix(">]"))
            .is_some_and(|path| path.trim().split('.').all(type_path_identifier));
    }
    if masked.split_whitespace().next() == Some("#nowarn") {
        return raw
            .strip_prefix("#nowarn")
            .filter(|argument| argument.starts_with(char::is_whitespace))
            .and_then(|argument| argument.trim().strip_prefix('"'))
            .and_then(|argument| argument.split_once('"'))
            .is_some_and(|(digits, suffix)| {
                !digits.is_empty()
                    && digits.bytes().all(|byte| byte.is_ascii_digit())
                    && (suffix.trim().is_empty() || suffix.trim_start().starts_with("//"))
            });
    }
    false
}

fn has_forth_definition(source: &str, truncated: bool, exhausted_sample: bool) -> bool {
    let mut colons = Vec::new();
    let mut heads = Vec::new();
    let mut offset = 0;
    for line in source.split_inclusive('\n') {
        let mut words = line.split_whitespace();
        if words.next() == Some(":") && words.next().is_some() {
            let colon = offset + line.len() - line.trim_start().len();
            let head = complete_function_return_annotation(source, colon, exhausted_sample);
            if colons.try_reserve(1).is_err() {
                return true;
            }
            colons.push((colon, head.is_some()));
            if let Some(head) = head {
                if heads.try_reserve(1).is_err() {
                    return true;
                }
                heads.push(head);
            }
        }
        offset += line.len();
    }
    // A complete finite head admits ';' only in constructor-owned named fields.
    // Its exact range excludes the annotation, body, and unrelated sampled tail.
    heads.sort_unstable_by_key(|head| (head.start, head.end));
    let mut heads = heads.iter().peekable();
    let mut owned_end = 0;
    let mut owned_separator = false;
    let has_terminator = source.char_indices().any(|(position, character)| {
        if character != ';'
            || !source
                .get(..position)
                .is_some_and(|before| before.chars().next_back().is_none_or(char::is_whitespace))
            || !source
                .get(position + 1..)
                .is_some_and(|after| after.chars().next().is_none_or(char::is_whitespace))
        {
            return false;
        }
        while let Some(head) = heads.peek()
            && head.start <= position
        {
            owned_end = owned_end.max(head.end);
            heads.next();
        }
        if position < owned_end {
            owned_separator = true;
            false
        } else {
            true
        }
    });
    // Ignoring a proved head separator cannot admit an unrelated true-EOF colon.
    colons.into_iter().any(|(colon, callable_owned)| {
        has_terminator
            || ((truncated || owned_separator)
                && !callable_owned
                && !complete_parenthesized_annotation(source, colon))
    })
}

fn complete_parenthesized_annotation(source: &str, colon: usize) -> bool {
    let Some(prefix) = source.get(..colon) else {
        return false;
    };
    let mut owners = Vec::new();
    let capacity = prefix
        .bytes()
        .filter(|byte| matches!(byte, b'(' | b'[' | b'{'))
        .count();
    if owners.try_reserve(capacity).is_err() {
        return false;
    }
    let mut index = 0;
    while let Some(byte) = prefix.as_bytes().get(index) {
        match delimiter_character(source, index) {
            DelimiterCharacter::Complete => {
                index += 3;
                continue;
            }
            DelimiterCharacter::Incomplete => return false,
            DelimiterCharacter::Absent => {}
        }
        match byte {
            b'(' | b'[' | b'{' => owners.push((*byte, index)),
            b')' | b']' | b'}' => {
                let expected = match byte {
                    b')' => b'(',
                    b']' => b'[',
                    _ => b'{',
                };
                if !owners.pop().is_some_and(|(owner, _)| owner == expected) {
                    return false;
                }
            }
            _ => {}
        }
        index += 1;
    }
    let Some((b'(', opening)) = owners.last().copied() else {
        return false;
    };
    if source
        .get(opening + 1..colon)
        .is_none_or(|expression| expression.trim().is_empty())
    {
        return false;
    }
    let mut index = colon + 1;
    while let Some(byte) = source.as_bytes().get(index) {
        match delimiter_character(source, index) {
            DelimiterCharacter::Complete => {
                index += 3;
                continue;
            }
            DelimiterCharacter::Incomplete => return false,
            DelimiterCharacter::Absent => {}
        }
        if *byte == b')' {
            // Only this finite, fully sampled type shape can remove conflicting
            // evidence; unknown type syntax remains ambiguous with Forth.
            let Some(annotation) = source.get(colon + 1..index) else {
                return false;
            };
            let annotation = annotation.trim();
            let path = match annotation.split_once('|') {
                Some((path, nullable)) if nullable.trim() == "null" => path.trim(),
                Some(_) => return false,
                None => annotation,
            };
            return path
                .split('.')
                .all(|part| type_path_identifier(part.trim()));
        }
        index += 1;
    }
    false
}

fn complete_function_return_annotation(
    source: &str,
    colon: usize,
    exhausted_sample: bool,
) -> Option<Range<usize>> {
    // Only complete sampled lines can prove a function owner. A partial tail
    // still supplies Forth conflicts, but cannot finish a return type or body.
    let end = source.rfind('\n')? + 1;
    let sampled_body_tail = source.get(end..).unwrap_or("");
    let source = source.get(..end)?;
    let before_colon = source.get(..colon)?;
    let line_start = before_colon.rfind('\n').map_or(0, |end| end + 1);
    let annotation_tail = source.get(line_start..)?;
    let (annotation_line, body) = annotation_tail.split_once('\n')?;
    let annotation_indent = space_indentation(annotation_line)?;
    let annotation = annotation_line.trim().strip_prefix(':')?;
    let (returned, inline_body) = annotation.split_once('=')?;
    let returned = match returned.trim().split_once('|') {
        Some((path, nullable)) if nullable.trim() == "null" => path.trim(),
        Some(_) => return None,
        None => returned.trim(),
    };
    if !finite_type_expression(returned) || inline_body.trim_start().starts_with(['=', '>']) {
        return None;
    }
    let before_annotation = source.get(..line_start)?;
    let mut head_start = before_annotation.len();
    for line in before_annotation.split_inclusive('\n').rev() {
        head_start -= line.len();
        if callable_head_rest(line.trim_start()).is_none() {
            continue;
        }
        let head_indent = space_indentation(line)?;
        let head = source.get(head_start..line_start)?;
        // The nearest callable must own the entire head. Do not skip an unrelated
        // completed binding to borrow an older function's parameter groups.
        if annotation_indent <= head_indent || !finite_function_head(head, head_indent) {
            return None;
        }
        if !inline_body.trim().is_empty() {
            return return_body_expression(inline_body.trim()).then_some(head_start..line_start);
        }
        let mut lines = body
            .lines()
            .filter(|line| !line.trim().is_empty())
            .peekable();
        let Some(first) = lines.next() else {
            // Only a truly exhausted sample may leave an owned body unfinished.
            // It can preserve an earlier independent anchor, never create one.
            return (exhausted_sample
                && space_indentation(sampled_body_tail)
                    .is_some_and(|indent| indent > head_indent)
                && return_body_expression(sampled_body_tail.trim())
                && prior_declaration_anchor(source.get(..head_start).unwrap_or("")))
            .then_some(head_start..line_start);
        };
        return (space_indentation(first).is_some_and(|indent| indent > head_indent)
            && if first.split_whitespace().next() == Some("let") {
                declaration(first, lines.peek().copied())
            } else {
                return_body_expression(first.trim())
            })
        .then_some(head_start..line_start);
    }
    None
}

fn prior_declaration_anchor(source: &str) -> bool {
    let mut lines = source
        .lines()
        .filter(|line| !line.trim().is_empty())
        .peekable();
    while let Some(line) = lines.next() {
        if declaration(line, lines.peek().copied()) {
            return true;
        }
    }
    false
}

fn finite_function_head(head: &str, head_indent: usize) -> bool {
    if head.lines().skip(1).any(|line| {
        !line.trim().is_empty()
            && space_indentation(line).is_none_or(|indent| indent <= head_indent)
    }) {
        return false;
    }
    let Some((owner, mut rest)) = callable_head_rest(head.trim_start()) else {
        return false;
    };
    loop {
        rest = rest.trim_start();
        let Some((word, following)) = rest.split_once(char::is_whitespace) else {
            return false;
        };
        if matches!(word, "rec" | "private" | "internal" | "public" | "inline") {
            rest = following;
            continue;
        }
        break;
    }
    let name_end = rest
        .find(|character: char| character.is_whitespace() || character == '<')
        .unwrap_or(rest.len());
    let name = &rest[..name_end];
    if !match owner {
        CallableOwner::Binding => type_path_identifier(name),
        CallableOwner::Member => finite_type_path(name),
    } {
        return false;
    }
    rest = &rest[name_end..];
    if rest.starts_with('<') {
        let Some(end) = balanced_group_end(rest, b'<', b'>') else {
            return false;
        };
        let generic = &rest[1..end];
        if !generic_head(generic) {
            return false;
        }
        rest = &rest[end + 1..];
        if !rest.starts_with(char::is_whitespace) && !rest.starts_with('(') {
            return false;
        }
    }
    finite_parameters(rest)
}

enum CallableOwner {
    Binding,
    Member,
}

fn callable_head_rest(line: &str) -> Option<(CallableOwner, &str)> {
    let (keyword, rest) = line.split_once(char::is_whitespace)?;
    match keyword {
        "let" | "and" => Some((CallableOwner::Binding, rest)),
        "member" | "override" | "default" => Some((CallableOwner::Member, rest)),
        "static" => rest
            .trim_start()
            .strip_prefix("member")
            .filter(|rest| rest.starts_with(char::is_whitespace))
            .map(|rest| (CallableOwner::Member, rest)),
        _ => None,
    }
}

fn generic_head(generic: &str) -> bool {
    // Constraints establish only a finite authored head, never type resolution.
    let (parameters, constraints) = split_keyword(generic, "when")
        .map_or((generic, None), |(parameters, constraints)| {
            (parameters, Some(constraints))
        });
    if !parameters.split(',').all(type_variable) {
        return false;
    }
    let Some(mut constraints) = constraints else {
        return true;
    };
    loop {
        let (constraint, following) = split_keyword(constraints, "and")
            .map_or((constraints, None), |(constraint, following)| {
                (constraint, Some(following))
            });
        let Some((variable, shape)) = constraint.split_once(':') else {
            return false;
        };
        if !type_variable(variable) {
            return false;
        }
        let shape = shape.trim();
        let valid = if let Some(subtype) = shape.strip_prefix('>') {
            let subtype = subtype.trim();
            let subtype = subtype.strip_suffix("| null").unwrap_or(subtype).trim_end();
            finite_type_expression(subtype)
        } else {
            matches!(
                shape,
                "comparison" | "equality" | "struct" | "not struct" | "unmanaged" | "null"
            )
        };
        if !valid {
            return false;
        }
        let Some(following) = following else {
            return true;
        };
        constraints = following;
    }
}

fn type_variable(variable: &str) -> bool {
    variable
        .trim()
        .strip_prefix(['\'', '^'])
        .is_some_and(type_path_identifier)
}

fn split_keyword<'a>(source: &'a str, keyword: &str) -> Option<(&'a str, &'a str)> {
    source.match_indices(keyword).find_map(|(offset, _)| {
        let before = &source[..offset];
        let after = &source[offset + keyword.len()..];
        (before.chars().next_back().is_some_and(char::is_whitespace)
            && after.chars().next().is_some_and(char::is_whitespace))
        .then_some((before, after))
    })
}

fn finite_parameters(mut parameters: &str) -> bool {
    let mut observed = false;
    loop {
        parameters = parameters.trim_start();
        if parameters.is_empty() {
            return observed;
        }
        if !parameters.starts_with('(') {
            let (name, rest) = parameters
                .split_once(char::is_whitespace)
                .unwrap_or((parameters, ""));
            if !type_path_identifier(name) {
                return false;
            }
            observed = true;
            parameters = rest;
            continue;
        }
        let Some(closing) = balanced_group_end(parameters, b'(', b')') else {
            return false;
        };
        let parameter = &parameters[1..closing];
        if !finite_parameter_group(parameter) {
            return false;
        }
        let Some(following) = parameters.get(closing + 1..) else {
            return false;
        };
        if !following.is_empty() && !following.starts_with(char::is_whitespace) {
            return false;
        }
        observed = true;
        parameters = following;
    }
}

fn finite_parameter_group(parameter: &str) -> bool {
    if parameter.trim().is_empty() {
        return true;
    }
    // Tupled parameters own separate annotations; commas inside a type or
    // nested pattern do not divide the outer constructor/function group.
    let mut delimiters = Vec::new();
    if delimiters.try_reserve(parameter.len()).is_err() {
        return false;
    }
    let mut start = 0;
    for (index, byte) in parameter.bytes().enumerate() {
        match byte {
            b'(' => delimiters.push(b')'),
            b'[' => delimiters.push(b']'),
            b'{' => delimiters.push(b'}'),
            b'<' => delimiters.push(b'>'),
            b'>' if index > 0 && matches!(parameter.as_bytes()[index - 1], b':' | b'-') => {}
            b')' | b']' | b'}' | b'>' => {
                if delimiters.pop() != Some(byte) {
                    return false;
                }
            }
            b',' if delimiters.is_empty() => {
                if !parameter.get(start..index).is_some_and(finite_parameter) {
                    return false;
                }
                start = index + 1;
            }
            _ => {}
        }
    }
    delimiters.is_empty() && parameter.get(start..).is_some_and(finite_parameter)
}

fn finite_parameter(parameter: &str) -> bool {
    let Some(parameter) = strip_pattern_attributes(parameter.trim()) else {
        return false;
    };
    if parameter.is_empty() {
        return false;
    }
    match parameter.split_once(':') {
        Some((name, parameter_type)) => {
            !name.trim().is_empty()
                && finite_pattern(name.trim())
                && finite_type_expression(parameter_type.trim())
        }
        None => finite_pattern(parameter.trim()),
    }
}

fn strip_pattern_attributes(mut pattern: &str) -> Option<&str> {
    while let Some(attribute) = pattern.strip_prefix("[<") {
        let (name, following) = attribute.split_once(">]")?;
        if !finite_type_path(name.trim()) {
            return None;
        }
        pattern = following.trim_start();
    }
    Some(pattern)
}

fn balanced_group_end(source: &str, opening: u8, closing: u8) -> Option<usize> {
    let mut depth = 0_usize;
    for (index, byte) in source.bytes().enumerate() {
        if byte == opening {
            depth += 1;
        } else if byte == closing
            && !(closing == b'>'
                && index > 0
                && matches!(source.as_bytes()[index - 1], b':' | b'-'))
        {
            depth = depth.checked_sub(1)?;
            if depth == 0 {
                return Some(index);
            }
        }
    }
    None
}

struct PatternFrame {
    constructor: bool,
    field_start: bool,
    named_field: bool,
    expects_name: bool,
}

fn finite_pattern(pattern: &str) -> bool {
    if pattern.is_empty() || type_path_identifier(pattern) {
        return true;
    }
    // Named assignments and separators require the current constructor frame;
    // surrounding tuples cannot borrow that ownership from a nested pattern.
    let mut stack: Vec<PatternFrame> = Vec::new();
    if stack.try_reserve(pattern.len()).is_err() {
        return false;
    }
    let mut rest = pattern;
    let mut needs_atom = true;
    let mut constructor = false;
    while !rest.is_empty() {
        let trimmed = rest.trim_start();
        let newline = rest[..rest.len() - trimmed.len()].contains('\n');
        rest = trimmed;
        if rest.is_empty() {
            break;
        }
        if needs_atom && rest.starts_with("[<") {
            if let Some(frame) = stack.last_mut() {
                if frame.expects_name {
                    return false;
                }
                frame.field_start = false;
            }
            let Some(following) = strip_pattern_attributes(rest) else {
                return false;
            };
            rest = following;
            continue;
        }
        match rest.as_bytes()[0] {
            b'(' if needs_atom || constructor => {
                if let Some(frame) = stack.last_mut() {
                    if frame.expects_name {
                        return false;
                    }
                    frame.field_start = false;
                }
                stack.push(PatternFrame {
                    constructor,
                    field_start: constructor,
                    named_field: false,
                    expects_name: false,
                });
                needs_atom = true;
                constructor = false;
                rest = &rest[1..];
            }
            b')' if !needs_atom => {
                if stack.pop().is_none_or(|frame| frame.expects_name) {
                    return false;
                }
                constructor = false;
                rest = &rest[1..];
            }
            b',' if !needs_atom => {
                needs_atom = true;
                constructor = false;
                rest = &rest[1..];
            }
            b';' if !needs_atom => {
                let Some(frame) = stack.last_mut() else {
                    return false;
                };
                if !frame.constructor || !frame.named_field {
                    return false;
                }
                frame.field_start = true;
                frame.named_field = false;
                frame.expects_name = true;
                needs_atom = true;
                constructor = false;
                rest = &rest[1..];
            }
            _ => {
                let end = rest
                    .find(|character: char| {
                        character.is_whitespace()
                            || matches!(character, '(' | ')' | ',' | '=' | ';')
                    })
                    .unwrap_or(rest.len());
                let token = &rest[..end];
                let following = rest[end..].trim_start();
                if !needs_atom
                    && newline
                    && type_path_identifier(token)
                    && following.starts_with('=')
                    && let Some(frame) = stack.last_mut()
                    && frame.constructor
                    && frame.named_field
                {
                    // Only a completed current named field can own the newline;
                    // nested tuples and ordinary whitespace cannot reset it.
                    frame.field_start = true;
                    frame.named_field = false;
                    frame.expects_name = true;
                    needs_atom = true;
                    constructor = false;
                }
                if end == 0 || (!needs_atom && !constructor) || !finite_type_path(token) {
                    return false;
                }
                if let Some(value) = following.strip_prefix('=') {
                    let Some(frame) = stack.last_mut() else {
                        return false;
                    };
                    if !needs_atom
                        || !frame.constructor
                        || !frame.field_start
                        || !type_path_identifier(token)
                    {
                        return false;
                    }
                    frame.field_start = false;
                    frame.named_field = true;
                    frame.expects_name = false;
                    constructor = false;
                    rest = value;
                    continue;
                }
                if let Some(frame) = stack.last_mut() {
                    if frame.expects_name {
                        return false;
                    }
                    frame.field_start = false;
                }
                constructor = token
                    .rsplit('.')
                    .next()
                    .and_then(|part| part.chars().next())
                    .is_some_and(char::is_uppercase);
                needs_atom = false;
                rest = &rest[end..];
            }
        }
    }
    !needs_atom && stack.is_empty()
}

fn finite_type_expression(source: &str) -> bool {
    // A nonrecursive token proof keeps classification independent of semantic
    // parsing. The caller's fixed byte sample bounds every token and delimiter.
    let mut stack = Vec::new();
    if stack.try_reserve(source.len()).is_err() {
        return false;
    }
    let mut rest = source.trim();
    let mut needs_atom = true;
    while !rest.is_empty() {
        rest = rest.trim_start();
        if rest.is_empty() {
            break;
        }
        if rest.starts_with("[]") && !needs_atom {
            rest = &rest[2..];
            continue;
        }
        if rest.starts_with("->") && !needs_atom {
            rest = &rest[2..];
            needs_atom = true;
            continue;
        }
        let byte = rest.as_bytes()[0];
        match byte {
            b'(' if needs_atom => {
                stack.push(b')');
                rest = &rest[1..];
            }
            b'<' if !needs_atom => {
                stack.push(b'>');
                rest = &rest[1..];
                needs_atom = true;
            }
            b')' | b'>' if !needs_atom => {
                if stack.pop() != Some(byte) {
                    return false;
                }
                rest = &rest[1..];
            }
            b'*' if !needs_atom => {
                rest = &rest[1..];
                needs_atom = true;
            }
            b',' if !needs_atom && !stack.is_empty() => {
                rest = &rest[1..];
                needs_atom = true;
            }
            _ => {
                let end = rest
                    .find(|character: char| {
                        character.is_whitespace()
                            || matches!(character, '(' | ')' | '<' | '>' | '*' | ',' | '[' | ']')
                    })
                    .unwrap_or(rest.len());
                let atom = &rest[..end];
                if end == 0 {
                    return false;
                }
                if !needs_atom {
                    if !matches!(atom, "list" | "option" | "seq" | "array" | "voption") {
                        return false;
                    }
                } else if atom == "struct" {
                    rest = &rest[end..];
                    if !rest.trim_start().starts_with('(') {
                        return false;
                    }
                    continue;
                } else if !finite_type_path(atom.strip_prefix(['\'', '^']).unwrap_or(atom)) {
                    return false;
                }
                needs_atom = false;
                rest = &rest[end..];
            }
        }
    }
    !needs_atom && stack.is_empty()
}

fn return_body_expression(expression: &str) -> bool {
    !expression
        .chars()
        .next()
        .is_none_or(|character| matches!(character, ':' | '=' | ')' | ']' | '}'))
        && !expression.split_whitespace().next().is_some_and(|word| {
            matches!(
                word,
                "module" | "namespace" | "type" | "let" | "and" | "open" | "member" | "val"
            )
        })
}

fn finite_type_path(path: &str) -> bool {
    path.split('.')
        .all(|part| type_path_identifier(part.trim()))
}

fn type_path_identifier(name: &str) -> bool {
    identifier(name)
        && !matches!(
            name,
            "abstract"
                | "and"
                | "as"
                | "assert"
                | "base"
                | "begin"
                | "class"
                | "const"
                | "default"
                | "delegate"
                | "do"
                | "done"
                | "downcast"
                | "downto"
                | "elif"
                | "else"
                | "end"
                | "exception"
                | "extern"
                | "false"
                | "finally"
                | "fixed"
                | "for"
                | "fun"
                | "function"
                | "global"
                | "if"
                | "in"
                | "inherit"
                | "inline"
                | "interface"
                | "internal"
                | "lazy"
                | "let"
                | "match"
                | "member"
                | "module"
                | "mutable"
                | "namespace"
                | "new"
                | "null"
                | "of"
                | "open"
                | "or"
                | "override"
                | "private"
                | "public"
                | "rec"
                | "return"
                | "sig"
                | "static"
                | "struct"
                | "then"
                | "to"
                | "true"
                | "try"
                | "type"
                | "upcast"
                | "use"
                | "val"
                | "void"
                | "when"
                | "while"
                | "with"
                | "yield"
                | "break"
                | "checked"
                | "component"
                | "constraint"
                | "continue"
                | "fori"
                | "include"
                | "mixin"
                | "parallel"
                | "params"
                | "process"
                | "protected"
                | "pure"
                | "sealed"
                | "tailcall"
                | "trait"
                | "virtual"
        )
}

enum DelimiterCharacter {
    Absent,
    Complete,
    Incomplete,
}

fn delimiter_character(source: &str, index: usize) -> DelimiterCharacter {
    let Some(tail) = source.as_bytes().get(index..) else {
        return DelimiterCharacter::Absent;
    };
    if tail.first() != Some(&b'\'') {
        return DelimiterCharacter::Absent;
    }
    // Identifier apostrophes cannot open a character, and a character's raw
    // delimiter cannot become a parenthesis owner or its closing delimiter.
    if source.get(..index).is_some_and(|prefix| {
        prefix
            .chars()
            .next_back()
            .is_some_and(|character| character.is_alphanumeric() || matches!(character, '_' | '\''))
    }) {
        return DelimiterCharacter::Absent;
    }
    let delimiter = |byte: &u8| matches!(byte, b'(' | b')' | b'[' | b']' | b'{' | b'}');
    if tail.get(1).is_some_and(delimiter) {
        if tail.get(2) == Some(&b'\'') {
            DelimiterCharacter::Complete
        } else {
            DelimiterCharacter::Incomplete
        }
    } else if tail.get(1) == Some(&b'\\') && tail.get(2).is_some_and(delimiter) {
        DelimiterCharacter::Incomplete
    } else {
        DelimiterCharacter::Absent
    }
}

fn has_forth_product_terminator(source: &str) -> bool {
    let mut words = source.split_whitespace().peekable();
    let mut definition = false;
    let mut previous = None;
    while let Some(word) = words.next() {
        if word == ":" {
            definition = words.peek().is_some_and(|name| identifier(name));
        } else if word == ";" {
            // Unlike valid annotations and record fields, a type product
            // cannot end at '*' before the statement terminator.
            if definition && previous == Some("*") {
                return true;
            }
            definition = false;
        }
        previous = Some(word);
    }
    false
}

fn module_or_namespace(line: &str) -> bool {
    let Some((keyword, rest)) = line.split_once(char::is_whitespace) else {
        return false;
    };
    if !matches!(keyword, "module" | "namespace") {
        return false;
    }
    let rest = rest.trim();
    let rest = if keyword == "module" {
        let (modifier, remainder) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
        if matches!(modifier, "private" | "internal" | "public") {
            remainder.trim_start()
        } else {
            rest
        }
    } else {
        rest
    };
    let rest = rest.strip_prefix("rec ").unwrap_or(rest);
    let name = if keyword == "module" {
        rest.strip_suffix('=').unwrap_or(rest).trim_end()
    } else {
        rest
    };
    if keyword == "module" && matches!(name, "private" | "internal" | "public" | "rec") {
        return false;
    }
    name.split('.').all(identifier)
}

fn declaration(line: &str, next_line: Option<&str>) -> bool {
    let Some((keyword, rest)) = line.trim().split_once(char::is_whitespace) else {
        return false;
    };
    if !matches!(keyword, "let" | "type") {
        return false;
    }
    let Some((left, right)) = rest.split_once('=') else {
        return false;
    };
    let right = right.trim();
    if right.starts_with('=') {
        return false;
    }
    if keyword == "type" {
        return implicit_declaration_head(keyword, left).is_some()
            && declaration_body(line, right, next_line);
    }
    let mut words = left.split_whitespace();
    let mut name = words.next();
    while name.is_some_and(|word| {
        matches!(
            word,
            "rec" | "mutable" | "private" | "internal" | "public" | "inline"
        )
    }) {
        name = words.next();
    }
    if !name.is_some_and(identifier) {
        return false;
    }
    declaration_body(line, right, next_line)
}

fn implicit_declaration(line: &str, next_line: Option<&str>, has_prologue: bool) -> bool {
    let Some((keyword, rest)) = line.trim().split_once(char::is_whitespace) else {
        return false;
    };
    if !matches!(keyword, "let" | "type") {
        return false;
    }
    let Some((head, body)) = rest.split_once('=') else {
        return false;
    };
    let Some(typed_or_generic) = implicit_declaration_head(keyword, head) else {
        return false;
    };
    // A plain binding without a scope or source prologue remains ambiguous
    // on this shared suffix. Authored types/generics supply an independent cue.
    (has_prologue || typed_or_generic) && declaration_body(line, body.trim(), next_line)
}

fn finite_implicit_declaration_prefix(line: &str) -> bool {
    let Some((keyword, rest)) = line.trim().split_once(char::is_whitespace) else {
        return false;
    };
    if !matches!(keyword, "let" | "type") {
        return false;
    }
    let Some((head, body)) = rest.split_once('=') else {
        return false;
    };
    if implicit_declaration_head(keyword, head) != Some(false) {
        return false;
    }
    let body = body.trim();
    let path = body != "_" && body.split('.').all(type_path_identifier);
    if keyword == "type" {
        return head.trim() != "_" && type_path_identifier(head.trim()) && path;
    }
    // The masked literal marker proves a complete literal, not its contents.
    // Compound and multiline RHSs need their own ownership proof before bridging.
    matches!(body, "\"" | "()" | "true" | "false" | "null") || path || {
        let digits = body.strip_prefix(['+', '-']).unwrap_or(body);
        !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit())
    }
}

fn implicit_declaration_head(keyword: &str, mut head: &str) -> Option<bool> {
    loop {
        head = head.trim_start();
        let (word, rest) = head.split_once(char::is_whitespace).unwrap_or((head, ""));
        if !matches!(
            word,
            "rec" | "mutable" | "private" | "internal" | "public" | "inline"
        ) {
            break;
        }
        head = rest;
    }
    let end = head
        .find(|character: char| character.is_whitespace() || matches!(character, '<' | '(' | ':'))
        .unwrap_or(head.len());
    if !type_path_identifier(&head[..end]) {
        return None;
    }
    let mut rest = &head[end..];
    let mut generic = false;
    if rest.starts_with('<') {
        let closing = balanced_group_end(rest, b'<', b'>')?;
        if !generic_head(&rest[1..closing]) {
            return None;
        }
        rest = &rest[closing + 1..];
        if !rest.is_empty()
            && !rest.starts_with(char::is_whitespace)
            && !rest.starts_with('(')
            && !rest.starts_with(':')
        {
            return None;
        }
        generic = true;
    }
    rest = rest.trim();
    if keyword == "type" {
        if rest.is_empty() {
            return Some(generic);
        }
        let mut attributed = false;
        while let Some(attribute) = rest.strip_prefix("[<") {
            let (name, following) = attribute.split_once(">]")?;
            // Only a complete path-only attribute is routing evidence. Masked
            // argument text cannot prove arbitrary attribute expressions.
            if !finite_type_path(name.trim()) {
                return None;
            }
            rest = following.trim_start();
            attributed = true;
        }
        if let Some((accessibility, following)) = rest.split_once(char::is_whitespace)
            && matches!(accessibility, "private" | "internal" | "public")
        {
            rest = following.trim();
        }
        // A self-reference belongs to a real constructor, not a bare type name.
        // Other trailing words cannot borrow the constructor's routing evidence.
        let closing = balanced_group_end(rest, b'(', b')')?;
        let constructor = rest.get(..closing + 1)?;
        if !rest.starts_with('(') || !finite_parameters(constructor) {
            return None;
        }
        let following = rest.get(closing + 1..)?.trim();
        if !following.is_empty()
            && !following
                .strip_prefix("as")
                .filter(|name| name.starts_with(char::is_whitespace))
                .is_some_and(|name| type_path_identifier(name.trim()))
        {
            return None;
        }
        return Some(generic || attributed || constructor.contains(':') || !following.is_empty());
    }
    // Only a top-level colon owns the return/value annotation. Parameter
    // annotations are validated with their complete parenthesized owner.
    let mut depth = 0_usize;
    let mut annotation = None;
    for (index, byte) in rest.bytes().enumerate() {
        match byte {
            b'(' => depth += 1,
            b')' => depth = depth.checked_sub(1)?,
            b':' if depth == 0 => {
                annotation = Some(index);
                break;
            }
            _ => {}
        }
    }
    let (parameters, returned) = annotation.map_or((rest, None), |colon| {
        (&rest[..colon], Some(rest[colon + 1..].trim()))
    });
    if !parameters.trim().is_empty() && !finite_parameters(parameters) {
        return None;
    }
    if returned.is_some_and(|returned| !finite_type_expression(returned)) {
        return None;
    }
    Some(generic || returned.is_some() || parameters.contains(':'))
}

fn declaration_body(line: &str, right: &str, next_line: Option<&str>) -> bool {
    if right.starts_with('=') {
        return false;
    }
    if !right.is_empty() {
        return true;
    }
    let Some(next_line) = next_line else {
        return false;
    };
    if next_line.trim_start().starts_with('=') {
        return false;
    }
    // Tabs have no declared offside width in this source-only recognizer.
    // Guessing one could turn an undecidable body into language evidence.
    match (space_indentation(line), space_indentation(next_line)) {
        (Some(head), Some(body)) => body > head,
        _ => false,
    }
}

fn space_indentation(line: &str) -> Option<usize> {
    let prefix = line.get(..line.len() - line.trim_start().len())?;
    prefix
        .bytes()
        .all(|byte| byte == b' ')
        .then_some(prefix.len())
}

fn identifier(name: &str) -> bool {
    let mut characters = name.chars();
    characters
        .next()
        .is_some_and(|character| character.is_alphabetic() || character == '_')
        && characters
            .all(|character| character.is_alphanumeric() || matches!(character, '_' | '\''))
}

fn mask_quoted_and_comment_contents(source: &[u8], truncated: bool) -> Option<Vec<u8>> {
    let mut masked = source.to_vec();
    let mut index = 0_usize;
    while let Some(tail) = source.get(index..) {
        if tail.is_empty() {
            break;
        }
        // Complete quote characters contain string delimiters, unlike ordinary
        // identifier/type-variable apostrophes. Do not search for a later tick.
        let quote_character_length = if tail.starts_with(b"'\"'") {
            3
        } else if tail.starts_with(b"'\\\"'") {
            4
        } else {
            0
        };
        let end = if quote_character_length != 0 {
            Some(index + quote_character_length)
        } else if tail.starts_with(b"``") {
            escaped_identifier_end(source, index)
        } else if tail.starts_with(b"//") {
            Some(
                index
                    + tail
                        .iter()
                        .position(|byte| *byte == b'\n')
                        .unwrap_or(tail.len()),
            )
        } else if tail.starts_with(b"(*") {
            block_comment_end(source, index)
        } else if tail.starts_with(b"\"") {
            string_end(source, index)
        } else {
            index += 1;
            continue;
        };
        let complete = end.is_some();
        let end = match end {
            Some(end) => end,
            // Preserve earlier evidence without completing a sampled name or literal.
            None if truncated => source.len(),
            None => return None,
        };
        for byte in masked.get_mut(index..end)? {
            if !matches!(*byte, b'\r' | b'\n') {
                *byte = b' ';
            }
        }
        if complete && tail.starts_with(b"``") {
            // Keep the existence of a complete name without letting its
            // escaped contents create mixed-language evidence.
            *masked.get_mut(index)? = b'_';
        } else if complete && (tail.starts_with(b"\"") || quote_character_length != 0) {
            // A complete literal can be a binding RHS; its contents cannot
            // create a header, declaration, or conflicting Forth definition.
            *masked.get_mut(index)? = b'"';
            if quote_character_length != 0 {
                // Preserve punctuation adjacency so a literal's following
                // separator cannot become a standalone Forth terminator.
                *masked.get_mut(end - 1)? = b'"';
            }
        }
        index = end;
    }
    Some(masked)
}

fn escaped_identifier_end(source: &[u8], start: usize) -> Option<usize> {
    let inner = source.get(start + 2..)?;
    let end = inner.windows(2).position(|bytes| bytes == b"``")?;
    let content = inner.get(..end)?;
    (!content.is_empty() && !content.iter().any(|byte| matches!(*byte, b'\r' | b'\n')))
        .then_some(start + end + 4)
}

fn block_comment_end(source: &[u8], start: usize) -> Option<usize> {
    let mut index = start + 2;
    let mut depth = 1_usize;
    while let Some(tail) = source.get(index..) {
        if tail.starts_with(b"(*") {
            depth += 1;
            index += 2;
        } else if tail.starts_with(b"*)") {
            depth -= 1;
            index += 2;
            if depth == 0 {
                return Some(index);
            }
        } else if tail.is_empty() {
            return None;
        } else {
            index += 1;
        }
    }
    None
}

fn string_end(source: &[u8], start: usize) -> Option<usize> {
    if source.get(start..)?.starts_with(b"\"\"\"") {
        let end = source
            .get(start + 3..)?
            .windows(3)
            .position(|bytes| bytes == b"\"\"\"")?;
        return Some(start + end + 6);
    }
    let verbatim = start.checked_sub(1).and_then(|index| source.get(index)) == Some(&b'@');
    let mut index = start + 1;
    while let Some(byte) = source.get(index) {
        if *byte == b'"' {
            if verbatim && source.get(index + 1) == Some(&b'"') {
                index += 2;
            } else {
                return Some(index + 1);
            }
        } else if !verbatim && *byte == b'\\' {
            index += 2;
        } else {
            index += 1;
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::mask_quoted_and_comment_contents;

    fn has_content_hint(source: &[u8], truncated: bool) -> bool {
        super::has_content_hint(
            source,
            truncated,
            truncated && source.len() == super::super::MAX_CLASSIFICATION_BYTES,
        )
    }

    #[test]
    fn fsharp_native_probe_allows_missing_hints_without_creating_evidence() {
        for source in [
            "let identity value = value\n",
            "let invoke () = 1\n",
            "let map = fun value -> value\n",
            "type Alias = System.String\n",
            "type Choice = First | Second\n",
            "type IValue =\n    abstract Value: int\n",
        ] {
            assert!(super::native_probe_allowed(source.as_bytes(), false, false));
            assert!(!has_content_hint(source.as_bytes(), false), "{source:?}");
        }
        for source in [
            "",
            "plain text\n",
            "// : square dup * ;\n",
            "(* : square dup * ; *)",
        ] {
            assert!(super::native_probe_allowed(source.as_bytes(), false, false));
        }
    }

    #[test]
    fn fsharp_native_probe_preserves_whole_sample_forth_vetoes() {
        for source in [
            ": square dup * ;\n",
            ": square dup *\n",
            "let identity value = value\n: square dup * ;\n",
            "let identity value = value\n: square dup *\n",
            "let answer = 1 : square dup * ;\n",
            "let answer = 1\n0 [if] : square dup * ; [then]\n",
            "let value\n    : int = 1\n",
        ] {
            for truncated in [false, true] {
                assert!(
                    !super::native_probe_allowed(source.as_bytes(), truncated, false),
                    "{source:?}, truncated={truncated}"
                );
            }
        }
        for source in [
            "let identity value\n    : int = value\n",
            "let identity (value\n    : int) = value\n",
            "let text = \": square dup * ;\"\n",
            "let ``: square dup * ;`` = 1\n",
            "let text = 1\n(* outer (* : square dup * ; *) inner *)\n",
        ] {
            assert!(
                super::native_probe_allowed(source.as_bytes(), false, false),
                "{source:?}"
            );
        }
    }

    #[test]
    fn fsharp_native_probe_rejects_incomplete_lexical_contexts_and_split_scalars() {
        for source in [
            b"let value = \"unfinished".as_slice(),
            b"let value = @\"unfinished",
            b"let value = \"\"\"unfinished",
            b"let value = 1\n(* unfinished",
            b"let value = 1\n(* outer (* inner *)",
            b"let ``unfinished = 1",
            b"let ```` = 1",
            b"let ``multi\nline`` = 1",
            b"let value = \xff",
            b"let value = \xce",
        ] {
            for truncated in [false, true] {
                assert!(!super::native_probe_allowed(source, truncated, false));
            }
        }
        let bound = super::super::MAX_CLASSIFICATION_BYTES;
        let oversized = vec![b' '; bound + 1];
        assert!(!super::native_probe_allowed(&oversized, true, true));
        let mut source = b"let identity value = value\n".to_vec();
        source.resize(bound - 1, b' ');
        source.extend_from_slice("\"closed outside the sample\"".as_bytes());
        assert!(!super::native_probe_allowed(&source[..bound], true, true));
        source.truncate(bound - 1);
        source.extend_from_slice("λ".as_bytes());
        assert!(!super::native_probe_allowed(&source[..bound], true, true));
        source.truncate(bound - 1);
        source.extend_from_slice(b" \nlet later = 1\n");
        assert!(super::native_probe_allowed(&source[..bound], true, true));
        let conflict = b"\n: square";
        source.truncate(bound - conflict.len());
        source.extend_from_slice(conflict);
        assert!(!super::native_probe_allowed(&source, true, true));
    }

    #[test]
    fn fsharp_explicit_type_heads_require_complete_generic_and_constructor_shapes() {
        for header in ["namespace Example\n", "module Example\n"] {
            for declaration in [
                "type Alias<'T> = 'T option\n",
                "type Holder(seed: int) =\n    member _.Value = seed\n",
                "type Holder<'T>(seed: 'T) =\n    member _.Value = seed\n",
                "type Pair(first: int, second: string) =\n    member _.First = first\n",
                "type Pair (first: int, second: string) =\n    member _.First = first\n",
                "type Pair(first: Example.Map<'T, 'U>, second: (int * string) list) = class end\n",
                "type Holder private (seed: int) =\n    member _.Value = seed\n",
                "type Marker() = class end\n",
                "type Marker [<ConstructorMarker>] () = class end\n",
                "type Holder [<Example.ConstructorMarker>] private (seed: int) = class end\n",
                "type Holder<'T> [<FirstMarker>] [<SecondMarker>] (seed: 'T) = class end\n",
                "type Holder(seed: int) as self = class end\n",
                "type Marker [<ConstructorMarker>] () as self = class end\n",
            ] {
                for newline in ["\n", "\r\n"] {
                    let source = format!("{header}{declaration}").replace('\n', newline);
                    assert!(has_content_hint(source.as_bytes(), false), "{source:?}");
                    assert!(has_content_hint(source.as_bytes(), true), "{source:?}");
                    let mixed = format!("{source}: square dup * ;{newline}");
                    assert!(!has_content_hint(mixed.as_bytes(), true), "{mixed:?}");
                }
            }
        }
        for declaration in [
            "type Alias<'T> trailing = 'T option\n",
            "type Holder(seed: int) extra = class end\n",
            "type Holder(seed: int)(extra: int) = class end\n",
            "type Holder(seed: int = class end\n",
            "type Holder(seed:) = class end\n",
            "type Holder(first: int, second:) = class end\n",
            "type Holder(first: int,) = class end\n",
            "type Holder(,second: int) = class end\n",
            "type Holder private = class end\n",
            "type Holder [<>] () = class end\n",
            "type Holder [<Marker>] = class end\n",
            "type Holder [<Marker>] () extra = class end\n",
            "type Holder [<Marker] () = class end\n",
            "type Holder [<Marker>] [<other words>] () = class end\n",
            "type Holder [<Marker(1)>] () = class end\n",
            "type Holder as this = class end\n",
            "type Holder as = class end\n",
            "type Holder() as = class end\n",
            "type Holder() as self extra = class end\n",
            "type Alias<'T>glued = 'T option\n",
            "type Alias<'T when> = 'T option\n",
            "type Holder(seed: int) =\n",
            "type Holder(seed: int) == class end\n",
        ] {
            for prefix in ["namespace Example\n", "open Example.Support\n", ""] {
                let source = format!("{prefix}{declaration}");
                assert!(!has_content_hint(source.as_bytes(), true), "{source:?}");
            }
        }
    }

    fn full_sample_with_tail(prefix: &str, tail: &str, newline: &str) -> Vec<u8> {
        let mut sample = prefix.as_bytes().to_vec();
        sample.extend_from_slice(b"//");
        sample.resize(
            super::super::MAX_CLASSIFICATION_BYTES - newline.len() - tail.len(),
            b'x',
        );
        sample.extend_from_slice(newline.as_bytes());
        sample.extend_from_slice(tail.as_bytes());
        assert_eq!(sample.len(), super::super::MAX_CLASSIFICATION_BYTES);
        sample
    }

    #[test]
    fn fsharp_prior_declarations_survive_an_owned_body_split_at_the_sample_boundary() {
        for newline in ["\n", "\r\n"] {
            let prefix = "module Example\nlet prior = 1\n".replace('\n', newline);
            for body in ["    va", "    match value wi", "    1", "    δ"] {
                let tail = format!("let identity (value: int)\n    : int =\n{body}")
                    .replace('\n', newline);
                let sample = full_sample_with_tail(&prefix, &tail, newline);
                assert!(has_content_hint(&sample, true), "{tail:?}");
            }
        }
    }

    #[test]
    fn fsharp_sampled_body_ownership_never_creates_an_anchor_or_hides_a_conflict() {
        let tail = "let identity (value: int)\n    : int =\n    va";
        let no_anchor = full_sample_with_tail("module Example\n", tail, "\n");
        assert!(!has_content_hint(&no_anchor, true));
        for tail in [
            "let identity (value: int)\n    : int =\n    ",
            "let identity (value: int)\n    : int =\n    // missing body",
            "let identity (value: int)\n    : int =\nva",
            "let identity (value: int)\n    : int =\n    module Other",
            "let identity (value: int)\n    : int =\n    let local =",
            "let identity (value: int)\n    : int =\n    va ;",
            "let identity (value: int)\n    : int =\n    : square",
            "let identity (: int)\n    : int =\n    va",
            "let identity (value: int)\n    : Example..Item =\n    va",
            "let identity (value: int)\n    : int =>\n    va",
            "let identity (value: int)\n    : int",
        ] {
            let sample = full_sample_with_tail("module Example\nlet prior = 1\n", tail, "\n");
            assert!(!has_content_hint(&sample, true), "{tail:?}");
        }
    }

    #[test]
    fn fsharp_implicit_modules_require_complete_typed_or_generic_heads_without_a_prologue() {
        for declaration in [
            "let answer: int = 1\n",
            "let identity (value: int) = value\n",
            "let identity (value: int) : int = value\n",
            "let inline identity<'T>(value: 'T) : 'T = value\n",
            "let combine<'T when 'T : comparison> (left: 'T) right = left\n",
            "type Box<'T> = Box of 'T\n",
            "type Counter(value: int) = member _.Value = value\n",
            "let answer: int =\n    1\n",
        ] {
            for newline in ["\n", "\r\n"] {
                let source = declaration.replace('\n', newline);
                assert!(has_content_hint(source.as_bytes(), false), "{source:?}");
                assert!(has_content_hint(source.as_bytes(), true), "{source:?}");
                let mixed = format!("{source}: square dup * ;{newline}");
                assert!(!has_content_hint(mixed.as_bytes(), false), "{mixed:?}");
                let partial = source.trim_end_matches(['\r', '\n']);
                if !partial.contains('\n') {
                    assert!(!has_content_hint(partial.as_bytes(), true), "{partial:?}");
                }
            }
        }
        for declaration in [
            "let answer = 1\n",
            "type Choice = Empty\n",
            "let answer: = 1\n",
            "let answer: int =\n",
            "let answer: int == 1\n",
            "let answer: int nonsense = 1\n",
            "let identity (value:) = value\n",
            "let identity (:int) = value\n",
            "let identity (value:int = value\n",
            "let identity<'T>glued (value:'T) = value\n",
            "let identity<'> (value:int) = value\n",
            "type Box<'T when> = Box of 'T\n",
            "type Box<'T>glued = Box of 'T\n",
            "foreign words\nlet identity (value:int) = value\n",
            "// let identity (value:int) = value\n",
            "\"let identity (value:int) = value\"\n",
        ] {
            assert!(
                !has_content_hint(declaration.as_bytes(), false),
                "{declaration:?}"
            );
        }
    }

    #[test]
    fn fsharp_implicit_atomic_prefixes_require_independent_later_evidence() {
        for prefix in [
            "let answer = 1\n",
            "let mutable answer = -12\n",
            "let text = \"module Hidden\\nlet fake:int=1\"\n",
            "let enabled = true\nlet empty = ()\n",
            "let first = 1\nlet second = 2\n",
            "let identity value = value\n",
            "type Alias = System.String\n",
        ] {
            for newline in ["\n", "\r\n"] {
                let source =
                    format!("{prefix}let echo (item: int) = item\n").replace('\n', newline);
                for truncated in [false, true] {
                    assert!(has_content_hint(source.as_bytes(), truncated), "{source:?}");
                }
                let mixed = format!("{source}: square dup * ;{newline}");
                assert!(!has_content_hint(mixed.as_bytes(), false), "{mixed:?}");
                assert!(!has_content_hint(mixed.as_bytes(), true), "{mixed:?}");
            }
            assert!(!has_content_hint(prefix.as_bytes(), false), "{prefix:?}");
        }
    }

    #[test]
    fn fsharp_implicit_prefixes_do_not_scan_past_unproved_ownership() {
        for prefix in [
            "let first =\n",
            "let first = 1 +\n",
            "let first = (1\n",
            "let first = foreign words\n",
            "let first = System.(* hidden *)String\n",
            "let first == 1\n",
            "let = 1\n",
            "let if = 1\n",
            "let first = _\n",
            "type Choice = ()\n",
            "type Choice = true\n",
            "type Choice = 1\n",
            "type Choice = \"text\"\n",
            "type _ = int\n",
            "type Missing<'T = int\n",
            "foreign words\n",
            "\"let first = 1\"\n",
        ] {
            let source = format!("{prefix}let echo (item: int) = item\n");
            assert!(!has_content_hint(source.as_bytes(), false), "{source:?}");
            assert!(!has_content_hint(prefix.as_bytes(), false), "{prefix:?}");
        }
        for source in [
            "let first = 1\n    let nested (item: int) = item\n",
            "    let first = 1\nlet unowned (item: int) = item\n",
            "let first = 1\nmodule Later\nlet answer = 2\n",
            "let first = 1\n#if FLAG\nlet echo (item: int) = item\n#endif\n",
        ] {
            assert!(!has_content_hint(source.as_bytes(), false), "{source:?}");
        }
    }

    #[test]
    fn fsharp_implicit_prefixes_keep_the_complete_line_and_sample_bound() {
        let partial = b"let first = 1\nlet echo (item: int) = item";
        assert!(has_content_hint(partial, false));
        assert!(!has_content_hint(partial, true));
        let mut outside = b"let first = 1\n".to_vec();
        outside.resize(super::super::MAX_CLASSIFICATION_BYTES, b' ');
        outside.extend_from_slice(b"\nlet echo (item: int) = item\n");
        assert!(!has_content_hint(
            &outside[..super::super::MAX_CLASSIFICATION_BYTES],
            true
        ));
        let sample = full_sample_with_tail(
            "let first = 1\nlet echo (item: int) = item\n",
            "// unrelated tail",
            "\n",
        );
        assert!(has_content_hint(&sample, true));
        let mixed = full_sample_with_tail(
            "let first = 1\nlet echo (item: int) = item\n",
            ": square dup * ;",
            "\n",
        );
        assert!(!has_content_hint(&mixed, true));
    }

    #[test]
    fn fsharp_implicit_modules_pair_a_source_prologue_with_a_complete_declaration() {
        for prologue in [
            "open Example.Support\n",
            "[<EntryPoint>]\n",
            "#nowarn \"42\" // scoped warning policy\n",
            "open Example.Support\n[<Example.Marker>]\n",
        ] {
            for declaration in [
                "let answer = 1\n",
                "let identity (value: int) = value\n",
                "let answer =\n    1\n",
                "type Entry =\n    { Count: int }\n",
                "type Choice =\n    | Empty\n    | Item of int\n",
            ] {
                for newline in ["\n", "\r\n"] {
                    let source = format!("\u{feff}// initial comment\n{prologue}{declaration}")
                        .replace('\n', newline);
                    assert!(has_content_hint(source.as_bytes(), false), "{source:?}");
                    assert!(has_content_hint(source.as_bytes(), true), "{source:?}");
                    let mut grown = source.into_bytes();
                    grown.extend_from_slice(b"// unrelated tail ");
                    grown.resize(super::super::MAX_CLASSIFICATION_BYTES + 1, b'x');
                    assert!(
                        has_content_hint(&grown[..super::super::MAX_CLASSIFICATION_BYTES], true,),
                        "{prologue:?}; {declaration:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn fsharp_implicit_module_evidence_does_not_promote_unowned_or_partial_text() {
        for source in [
            "open Example.Support\n",
            "[<EntryPoint>]\n",
            "#nowarn \"42\"\n",
            "let answer = 1\n",
            "open Example.Support\nlet answer =\n",
            "open Example.Support\nlet answer == 1\n",
            "open Example.Support\nlet answer =\n1\n",
            "open Example.Support\n// let answer = 1\n",
            "open Example.Support\n(* let answer = 1 *)\n",
            "\"open Example.Support\\nlet answer = 1\"\n",
            "open Example.Support\n\"let answer = 1\"\n",
            "open Ex(* hidden *)ample.Support\nlet answer = 1\n",
            "[<Ex(* hidden *)ample.Marker>]\nlet answer = 1\n",
            "foreign words\nopen Example.Support\nlet answer = 1\n",
            "open Example.Support\nlet answer = 1\n: square dup * ;\n",
            "open Example.Support\nlet answer = 1 : square dup * ;\n",
            "open Example.Support\nlet answer = 1\n0 [if] : square dup * ; [then]\n",
        ] {
            for truncated in [false, true] {
                assert!(
                    !has_content_hint(source.as_bytes(), truncated),
                    "{source:?}"
                );
            }
        }
        for source in [
            "open Example.Support\nlet answer = 1",
            "[<EntryPoint>]\nlet main _ = 0",
            "open Example.Support\nlet answer = 1\n: square",
            "open Example.Support\nlet answer =\n    1",
        ] {
            assert!(!has_content_hint(source.as_bytes(), true), "{source:?}");
        }
    }

    #[test]
    fn fsharp_return_annotations_recognize_generic_pattern_and_member_owners() {
        for (head, returned, body) in [
            (
                "let map<'Item, 'Result when 'Item : comparison>\n    (items: 'Item[])\n    (transform: 'Item -> 'Result)",
                "('Item * 'Result)[]",
                "[||]",
            ),
            ("let identity<'T>(value: 'T)", "'T", "value"),
            (
                "let visit\n    (Entry(value, metadata))\n    state",
                "Example.Result list",
                "[state]",
            ),
            (
                "    member inline _.Combine\n        (left: Example.Code<'State, 'T>)\n        (right: Example.Code<'State, 'T>)",
                "Example.Code<'State, 'T>",
                "        left",
            ),
            (
                "    static member Convert<'T when 'T :> Example.Item>\n        (value: 'T)",
                "Example.Item option",
                "        Some value",
            ),
        ] {
            for newline in ["\n", "\r\n"] {
                let indent = if head.starts_with(' ') {
                    "        "
                } else {
                    "    "
                };
                let source = format!(
                    "module Example\nlet prior = 1\n{head}\n{indent}: {returned} =\n{indent}{body}\n"
                )
                .replace('\n', newline);
                assert!(has_content_hint(source.as_bytes(), false), "{source:?}");
                assert!(has_content_hint(source.as_bytes(), true), "{source:?}");
                let mut grown = source.into_bytes();
                grown.extend_from_slice(b"// unrelated tail ");
                grown.resize(super::super::MAX_CLASSIFICATION_BYTES + 1, b'x');
                assert!(
                    has_content_hint(&grown[..super::super::MAX_CLASSIFICATION_BYTES], true),
                    "{head:?}"
                );
            }
        }
    }

    #[test]
    fn fsharp_sampled_many_owned_returns_keep_the_byte_bound_and_forth_conflicts() {
        use std::fmt::Write as _;

        let mut source = String::from("module Example\nlet prior = 1\n");
        for index in 0..100 {
            writeln!(source, "let function{index} value\n    : int =\n    value").unwrap();
        }
        assert!(source.len() < super::super::MAX_CLASSIFICATION_BYTES);
        assert!(has_content_hint(source.as_bytes(), true));
        let mut mixed = source.clone();
        mixed.push_str(": square\n");
        assert!(!has_content_hint(mixed.as_bytes(), true));
        source.push_str("// unrelated tail ");
        source.push_str(&"x".repeat(super::super::MAX_CLASSIFICATION_BYTES));
        assert!(has_content_hint(
            &source.as_bytes()[..super::super::MAX_CLASSIFICATION_BYTES],
            true,
        ));
        assert!(!has_content_hint(source.as_bytes(), true));
    }

    #[test]
    fn fsharp_nowarn_prologue_accepts_a_complete_trailing_line_comment() {
        for directive in [
            "#nowarn \"0042\" // scoped warning policy",
            "#nowarn\t\"42\"\t// module Ghost : word ;",
            "#nowarn \"42\"// trailing comment",
        ] {
            for newline in ["\n", "\r\n"] {
                let source =
                    format!("{directive}\nmodule Example\nlet value = 1\n").replace('\n', newline);
                assert!(has_content_hint(source.as_bytes(), false), "{source:?}");
                assert!(has_content_hint(source.as_bytes(), true), "{source:?}");
            }
        }
        for directive in [
            "#nowarn \"42\" extra // comment",
            "#nowarn \"42\" / / comment",
            "#nowarn \"4//2\"",
            "#nowarn \"42\" \"43\" // comment",
            "#nowarn \"42\" (* trailing block comment *)",
        ] {
            let source = format!("{directive}\nmodule Example\nlet value = 1\n");
            assert!(!has_content_hint(source.as_bytes(), false), "{source:?}");
        }
    }

    #[test]
    fn fsharp_sampled_parenthesized_annotations_retain_prior_evidence() {
        for literal in ["'\"'", "'\\\"'"] {
            for newline in ["\n", "\r\n"] {
                for annotation in ["int", "Example.Item", "Example.Item | null"] {
                    let source = format!(
                        "module Example\nlet prior = 1\nlet quote = {literal}\n\
                         let value =\n    (1\n    : {annotation})\n"
                    )
                    .replace('\n', newline);
                    assert!(has_content_hint(source.as_bytes(), true), "{source:?}");
                }
                let source = format!(
                    "module Example\nlet prior = 1\nlet quote = {literal}\n\
                     let value =\n    (match None with\n     | None -> \"\"\n\
                         | Some item -> item\n    : string | null)\n"
                )
                .replace('\n', newline);
                assert!(has_content_hint(source.as_bytes(), true), "{source:?}");
            }
        }
    }

    #[test]
    fn fsharp_sampled_annotation_ownership_ignores_literal_delimiters() {
        for expression in [
            "(1",
            "('('",
            "(')'",
            "('['",
            "(']'",
            "('{'",
            "('}'",
            "([ '('; ')'; '['; ']'; '{'; '}' ]",
            "call'(1",
        ] {
            let source = format!(
                "module Example\nlet prior = 1\nlet quote = '\"'\n\
                 let value =\n    {expression}\n    : Example.Item)\n"
            );
            assert!(has_content_hint(source.as_bytes(), true), "{source:?}");
        }
    }

    #[test]
    fn fsharp_sampled_annotation_proof_rejects_missing_or_false_ownership() {
        for tail in [
            ": square\n",
            ": square",
            ": int)\n",
            "let opener = '('\n: int)\n",
            "let opener = '(\n: int)\n",
            "let opener = '\\('\n: int)\n",
            "let value = (\n: int)\n",
            "let value = ([1\n: int)\n",
            "let value = (1\n: int\n",
            "let value = (1\n: int // )\n",
            "let value = (1\n: int (* ) *)\n",
            "let value = (1\n: int \" ) \"\n",
            "let value = (1\n: int ')'\n",
            "let value = (1\n: int list)\n",
            "let value = (1\n: int * int)\n",
            "let value = (1\n: Example..Item)\n",
            "let value = (1\n: Example.Item | other)\n",
            "let value = (1\n: Example.Item | null extra)\n",
        ] {
            let source = format!("module Example\nlet prior = 1\nlet quote = '\"'\n{tail}");
            assert!(!has_content_hint(source.as_bytes(), true), "{source:?}");
        }
    }

    #[test]
    fn fsharp_return_annotations_survive_a_grown_bounded_tail() {
        let bound = super::super::MAX_CLASSIFICATION_BYTES;
        for newline in ["\n", "\r\n"] {
            let source = concat!(
                "module Example\nlet prior = 1\n",
                "let combine\n",
                "    (entries: Example.Entry list)\n",
                "    (choice: Example.Entry option)\n",
                "    (pairs: (int * string) list)\n",
                "    (counts: int[])\n",
                "    : Example.Result =\n",
                "    let value =\n        prior\n    value\n",
            )
            .replace('\n', newline);
            assert!(has_content_hint(source.as_bytes(), false), "{source:?}");
            let mut grown = source.into_bytes();
            grown.extend_from_slice(b"// unrelated tail ");
            grown.resize(bound + 1, b'x');
            assert!(has_content_hint(&grown[..bound], true), "{newline:?}");
            assert!(!has_content_hint(&grown, true));
        }
    }

    #[test]
    fn fsharp_return_annotations_accept_only_finite_complete_parameter_forms() {
        for parameter in [
            "int",
            "Example.Item",
            "Example.Item list",
            "Example.Item option",
            "int[]",
            "(int * string)",
            "(int * Example.Item * string) list",
            "int -> int",
            "Example.Item<int>",
            "(int * (string * int))",
        ] {
            for returned in ["int", "Example.Item", "Example.Item | null"] {
                let source = format!(
                    "module Example\nlet prior = 1\nlet private combine\n    (value: {parameter})\n    : {returned} =\n    prior\n"
                );
                assert!(has_content_hint(source.as_bytes(), true), "{source:?}");
            }
        }
        let inline_parameter = "module Example\nlet prior = 1\n\
            let combine (value: int)\n    : int =\n    value\n";
        assert!(has_content_hint(inline_parameter.as_bytes(), true));
        for parameters in [
            "(left: int, right: int)",
            "(left: Map<string, int>, right: (int * string) list)",
            "(left: int -> int, right: int[])",
            "((left, right): int * int)",
        ] {
            let source = format!(
                "module Example\nlet prior = 1\nlet combine {parameters}\n    : int =\n    prior\n"
            );
            assert!(has_content_hint(source.as_bytes(), true), "{source:?}");
            let conflict = format!("{source}: square dup * ;\n");
            assert!(!has_content_hint(conflict.as_bytes(), true));
        }
    }

    #[test]
    fn fsharp_sampled_callable_named_field_patterns_preserve_prior_evidence() {
        let bound = super::super::MAX_CLASSIFICATION_BYTES;
        for (prefix, head, indent) in [
            ("module Example\nlet prior = 1\n", "let combine", "    "),
            (
                "namespace Example\ntype Holder() =\n",
                "    member _.Combine",
                "        ",
            ),
        ] {
            for parameter in [
                "(Packet(item=value))",
                "(Example.Packet(item=value; metadata=state))",
                "(Example.Packet(item = value ; metadata = state))",
                "(Packet(item=Nested(key=value); metadata=(left, right)))",
                "(Packet(item=value): Example.Packet)",
            ] {
                for newline in ["\n", "\r\n"] {
                    let source = format!(
                        "{prefix}{head}\n{indent}{parameter}\n{indent}: int =\n{indent}1\n"
                    )
                    .replace('\n', newline);
                    let mut grown = source.into_bytes();
                    grown.extend_from_slice(b"// padded tail ");
                    grown.resize(bound + 1, b'x');
                    assert!(
                        has_content_hint(&grown[..bound], true),
                        "{head:?}; {parameter:?}; {newline:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn fsharp_multiline_named_fields_preserve_sampled_callable_owners() {
        let bound = super::super::MAX_CLASSIFICATION_BYTES;
        for (prefix, head, indent) in [
            ("module Example\nlet prior = 1\n", "let combine", "    "),
            (
                "namespace Example\ntype Holder() =\n",
                "    member _.Combine",
                "        ",
            ),
        ] {
            for parameter in [
                "(Packet(item=value\n        metadata=state))",
                "(Example.Packet(item=(left, right)\n        metadata=state): Example.Packet)",
                "(Packet(item=Nested(key=value\n        code=state)\n        metadata=state))",
                "(Packet(item=value\n        metadata=state ; code=other))",
            ] {
                for newline in ["\n", "\r\n"] {
                    let source = format!(
                        "{prefix}{head}\n{indent}{parameter}\n{indent}: int =\n{indent}1\n"
                    )
                    .replace('\n', newline);
                    let mut grown = source.into_bytes();
                    grown.extend_from_slice(b"// padded tail ");
                    grown.resize(bound + 1, b'x');
                    assert!(
                        has_content_hint(&grown[..bound], true),
                        "{head:?}; {parameter:?}; {newline:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn fsharp_multiline_named_fields_require_completed_current_constructor() {
        for pattern in [
            "Packet(item=value\n metadata=state)",
            "Packet(item=(left, right)\n metadata=state)",
            "Packet(item=Nested(key=value\n code=state)\n metadata=state)",
        ] {
            assert!(super::finite_pattern(pattern), "{pattern:?}");
        }
        for pattern in [
            "Packet(item=value metadata=state)",
            "Packet(item=value\r metadata=state)",
            "Packet(item=value\u{2028} metadata=state)",
            "Packet(item=value\n other)",
            "Packet(value\n metadata=state)",
            "Packet(item=\n metadata=state)",
            "Packet(item=value\n metadata=)",
            "Packet(item=value\n metadata==state)",
            "Packet(item=value\n Item.Name=state)",
            "packet(item=value\n metadata=state)",
            "(item=value\n metadata=state)",
            "(Packet(item=value)\n metadata=state)",
            "Packet(item=(left\n metadata=state))",
            "Packet(item=Nested(value\n metadata=state))",
            "Packet(item=value\n metadata=state",
        ] {
            assert!(!super::finite_pattern(pattern), "{pattern:?}");
        }
    }

    #[test]
    fn fsharp_multiline_named_fields_keep_masked_field_ownership() {
        let bound = super::super::MAX_CLASSIFICATION_BYTES;
        for (parameter, recognized) in [
            (
                "(Packet(item=value\n        (* field trivia *) metadata=state))",
                true,
            ),
            (
                "(Packet(item=value \"hidden\n        metadata=state\"))",
                false,
            ),
            (
                "(Packet(item=value\n        metadata (* = state *) ))",
                false,
            ),
            (
                "(Packet(item=value (*\n        metadata=state *) extra))",
                false,
            ),
            (
                "(Packet(item=value\n        metadata // = state\n        ))",
                false,
            ),
            (
                "(Packet(item=\n        (* field trivia *) metadata=state))",
                false,
            ),
        ] {
            for newline in ["\n", "\r\n"] {
                let source = format!("module Example\nlet prior = 1\nlet combine\n    {parameter}\n    : int =\n    1\n").replace('\n', newline);
                let mut grown = source.into_bytes();
                grown.extend_from_slice(b"// padded tail ");
                grown.resize(bound + 1, b'x');
                assert_eq!(
                    has_content_hint(&grown[..bound], true),
                    recognized,
                    "{parameter:?}; {newline:?}"
                );
            }
        }
    }

    #[test]
    fn fsharp_multiline_named_fields_keep_forth_vetoes() {
        let bound = super::super::MAX_CLASSIFICATION_BYTES;
        let function = "module Example\nlet prior = 1\nlet combine\n    (Packet(item=value\n        metadata=state ; code=other))\n    : int =\n    1\n";
        for newline in ["\n", "\r\n"] {
            let function = function.replace('\n', newline);
            assert!(has_content_hint(function.as_bytes(), false));
            for tail in [": word\n", ": word dup * ;\n", ";\n"] {
                let tail = tail.replace('\n', newline);
                let source = format!("{function}{tail}");
                assert!(!has_content_hint(source.as_bytes(), false), "{tail:?}");
                let mut grown = source.into_bytes();
                grown.extend_from_slice(b"// padded tail ");
                grown.resize(bound + 1, b'x');
                assert!(!has_content_hint(&grown[..bound], true), "{tail:?}");
            }
            for body in ["dup * ;", "value ;"] {
                let source =
                    function.replace(&format!("    1{newline}"), &format!("    {body}{newline}"));
                assert!(!has_content_hint(source.as_bytes(), false), "{body:?}");
                let mut grown = source.into_bytes();
                grown.extend_from_slice(b"// padded tail ");
                grown.resize(bound + 1, b'x');
                assert!(!has_content_hint(&grown[..bound], true), "{body:?}");
            }
        }
    }

    #[test]
    fn fsharp_sampled_callable_parameter_attributes_preserve_prior_evidence() {
        let bound = super::super::MAX_CLASSIFICATION_BYTES;
        for (prefix, head, indent) in [
            ("module Example\nlet prior = 1\n", "let combine", "    "),
            (
                "namespace Example\ntype Holder() =\n",
                "    member _.Combine",
                "        ",
            ),
        ] {
            for parameter in [
                "([<Example.Marker>] value: int)",
                "([<FirstMarker>] [<Example.SecondMarker>] value: int)",
                "([<Example.Marker>] (left, right): int * int)",
                "([<Example.Marker>] Packet(value))",
            ] {
                for newline in ["\n", "\r\n"] {
                    let source = format!(
                        "{prefix}{head}\n{indent}{parameter}\n{indent}: int =\n{indent}1\n"
                    )
                    .replace('\n', newline);
                    let mut grown = source.into_bytes();
                    grown.extend_from_slice(b"// padded tail ");
                    grown.resize(bound + 1, b'x');
                    assert!(
                        has_content_hint(&grown[..bound], true),
                        "{head:?}; {parameter:?}; {newline:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn fsharp_sampled_callable_pattern_admission_preserves_negative_barriers() {
        let bound = super::super::MAX_CLASSIFICATION_BYTES;
        for parameter in [
            "(item=value)",
            "(item=value, state)",
            "(Packet(item=value) = state)",
            "(packet(item=value))",
            "(Packet(=value))",
            "(Packet(item=))",
            "(Packet(item==value))",
            "(Packet(item=value other))",
            "(Packet(Item.Name=value))",
            "(Packet(item=value;))",
            "(Packet(item=value;; metadata=state))",
            "(Packet(item=value; metadata=))",
            "(Packet(item=value)",
            "([<>] value: int)",
            "([<Example..Marker>] value: int)",
            "([<Example.Marker] value: int)",
            "([<Example.Marker(1)>] value: int)",
            "([<Example.Marker(\"text\")>] value: int)",
            "([<assembly: Example.Marker>] value: int)",
            "([<Example.Marker; Other.Marker>] value: int)",
            "([<Example.Marker>] : int)",
            "([<Example.Marker>] value int)",
        ] {
            let source = format!(
                "module Example\nlet prior = 1\nlet combine\n    {parameter}\n    : int =\n    1\n"
            );
            let mut grown = source.into_bytes();
            grown.extend_from_slice(b"// padded tail ");
            grown.resize(bound + 1, b'x');
            assert!(!has_content_hint(&grown[..bound], true), "{parameter:?}");
        }
        for head in [
            "let",
            "let (Packet(item=value))",
            "let combine (Packet(item=value)) = 1",
        ] {
            let source = format!("module Example\nlet prior = 1\n{head}\n    : int =\n    1\n");
            let mut grown = source.into_bytes();
            grown.extend_from_slice(b"// padded tail ");
            grown.resize(bound + 1, b'x');
            assert!(!has_content_hint(&grown[..bound], true), "{head:?}");
        }
        for tail in [
            "let combine\n    (Packet(item=value))\n    : int =\n",
            "let combine\n    ([<Example.Marker>] value: int)\n    : int =\n    // missing body\n",
            "let combine\n    (Packet(item=value))\n    : int\n    1\n",
        ] {
            let source = format!("module Example\nlet prior = 1\n{tail}");
            let mut grown = source.into_bytes();
            grown.extend_from_slice(b"// padded tail ");
            grown.resize(bound + 1, b'x');
            assert!(!has_content_hint(&grown[..bound], true), "{tail:?}");
        }
        let function = "module Example\nlet prior = 1\nlet combine\n    (Packet(item=value))\n    : int =\n    1\n";
        for tail in [";\n", ": word dup * ;\n", ": word\n", ": word"] {
            let sample = full_sample_with_tail(function, tail, "\n");
            let mut grown = sample;
            grown.push(b'x');
            assert!(!has_content_hint(&grown[..bound], true), "{tail:?}");
        }
        let no_anchor =
            "module Example\nlet combine\n    (Packet(item=value))\n    : int =\n    va";
        let sample = full_sample_with_tail("", no_anchor, "\n");
        let mut grown = sample;
        grown.push(b'l');
        assert!(!has_content_hint(&grown[..bound], true));
    }

    #[test]
    fn fsharp_sampled_callable_head_ranges_exclude_annotation_body_and_tail() {
        let source = "module Example\nlet prior = 1\nlet combine\n    (Packet(item = value ; metadata = state))\n    : int =\n    dup + ;\n: next";
        let start = source.find("let combine").unwrap();
        let end = source.find("\n    : int").unwrap() + 1;
        let colon = end + "    ".len();
        let proof = super::complete_function_return_annotation(source, colon, false).unwrap();
        assert_eq!(proof, start..end);
        assert_eq!(
            &source[proof],
            "let combine\n    (Packet(item = value ; metadata = state))\n"
        );
        assert!(super::has_forth_definition(source, true, false));
    }

    #[test]
    fn fsharp_sampled_callable_multiple_owned_separators_preserve_forth_vetoes() {
        let bound = super::super::MAX_CLASSIFICATION_BYTES;
        let functions = "module Example\nlet prior = 1\nlet first\n    (Packet(item = value ; metadata = state))\n    : int =\n    1\nlet second\n    ([<Example.Marker>] Packet(item = value ; metadata = state))\n    : int =\n    2\n";
        let mut grown = functions.as_bytes().to_vec();
        grown.extend_from_slice(b"// padded tail ");
        grown.resize(bound + 1, b'x');
        assert!(has_content_hint(&grown[..bound], true));
        for tail in [";\n", ": word dup + ;\n", ": word\n", ": word"] {
            assert!(functions.len() + tail.len() < bound);
            assert!(
                !has_content_hint(format!("{functions}{tail}").as_bytes(), false),
                "{tail:?}"
            );
            let mut grown = full_sample_with_tail(functions, tail, "\n");
            grown.push(b'x');
            assert!(!has_content_hint(&grown[..bound], true), "{tail:?}");
        }
        for body in ["dup + ;", "dup * ;", "value ;"] {
            let source = format!(
                "module Example\nlet prior = 1\nlet combine\n    (Packet(item = value ; metadata = state))\n    : int =\n    {body}\n"
            );
            let mut grown = source.into_bytes();
            grown.extend_from_slice(b"// padded tail ");
            grown.resize(bound + 1, b'x');
            assert!(!has_content_hint(&grown[..bound], true), "{body:?}");
        }
        let source = "module Example\nlet prior = 1\nlet combine\n    (Packet(item = value ; metadata = state))\n    : int = value ;\n";
        let mut grown = source.as_bytes().to_vec();
        grown.extend_from_slice(b"// padded tail ");
        grown.resize(bound + 1, b'x');
        assert!(!has_content_hint(&grown[..bound], true));
    }

    #[test]
    fn fsharp_sampled_callable_named_separators_require_the_current_constructor() {
        for pattern in [
            "Packet(item = value ; metadata = state)",
            "Packet(item=Nested(key=value ; code=state) ; metadata=(left, right))",
        ] {
            assert!(super::finite_pattern(pattern), "{pattern:?}");
        }
        for pattern in [
            "(item=value)",
            "Packet(item=(value ; other))",
            "(Packet(item=value) ; other)",
            "Packet(value ; other)",
            "Packet(item=value ; other)",
            "Packet(item=value ; metadata=)",
            "Packet(item=value ; ; metadata=state)",
            "Packet(item=value ; metadata=state);",
        ] {
            assert!(!super::finite_pattern(pattern), "{pattern:?}");
        }
    }

    #[test]
    fn fsharp_return_annotations_reject_unproven_owners() {
        for tail in [
            "let combine<'>\n    (value: int)\n    : int =\n    1\n",
            "let combine<'T arbitrary words>\n    (value: int)\n    : int =\n    1\n",
            "let combine<'T : square dup *>\n    (value: int)\n    : int =\n    1\n",
            "let combine<'T>x\n    : int =\n    1\n",
            "let combine<'T>let\n    (value: int)\n    : int =\n    1\n",
            "let Foreign.Words value\n    : int =\n    1\n",
            "let combine\n    (: int)\n    : int =\n    1\n",
            "let combine\n    (left right, extra)\n    : int =\n    1\n",
            "let combine\n    (value: int)\n    : Example.Item<> =\n    1\n",
            "let combine\n    (value: int)\n    : (int * ) =\n    1\n",
            "let combine\n    (value: int)\n    : struct Item =\n    1\n",
            "let combine\n    (value: int)\n    : int => 1\n",
            "let combine = (1: int)\n    : int =\n    1\n",
            "let combine (value: int) = value\n    : int =\n    1\n",
            "let combine\n    (value: int)\nmodule Other\n    : int =\n    1\n",
            "let combine\n(value: int)\n    : int =\n    1\n",
            "let combine\n    (value: int)\n: int =\n    1\n",
            "let combine\n\t(value: int)\n    : int =\n    1\n",
            "let combine\n    (value int)\n    : int =\n    1\n",
            "let combine\n    (left: int, right:)\n    : int =\n    1\n",
            "let combine\n    (left: int, right: Map<string, int])\n    : int =\n    1\n",
            "let combine\n    (left: int, right: int,)\n    : int =\n    1\n",
            "let combine\n    (value: int\n    : int =\n    1\n",
            "let combine\n    (value: int)\n    : Example..Item =\n    1\n",
            "let combine\n    (value: int)\n    : if =\n    1\n",
            "let combine\n    (value: int)\n    : int | other =\n    1\n",
            "let combine\n    (value: int)\n    : int =\n",
            "let combine\n    (value: int)\n    : int =\n    // missing body\n",
            "let combine\n    (value: int)\n    : int =\n1\n",
            "let combine\n    (value: int)\n    : int =\n    module Other\n",
            "let combine\n    (value: int)\n    : int =\n    let local =\n",
            "let combine\n    (value: int)\n    : int =\n    let local =\n    1\n",
            "let combine\n    (value: int)\n    : int =\n    1",
            "let combine\n    (value: int)\n    : int = 1",
        ] {
            let source = format!("module Example\nlet prior = 1\n{tail}");
            assert!(!has_content_hint(source.as_bytes(), true), "{source:?}");
        }
    }

    #[test]
    fn fsharp_return_annotations_preserve_visible_forth_vetoes() {
        let function = "module Example\nlet prior = 1\n\
            let combine (value: int)\n    : int =\n    value\n";
        for tail in [";\n", ": square dup * ;\n", ": square\n", ": square"] {
            let source = format!("{function}{tail}");
            assert!(!has_content_hint(source.as_bytes(), true), "{source:?}");
        }
    }

    #[test]
    fn fsharp_sampled_annotations_do_not_exempt_forth_terminators() {
        for tail in [
            "let value = (1\n: int) ;\n",
            "let value = (1\n: square dup ;)\n",
            "let value = (1\n: square dup * ;)\n",
            "let value = (1\n: int)\n: square",
            "let value = (1\n: int)\n: square dup ;\n",
        ] {
            let source = format!("module Example\nlet prior = 1\nlet quote = '\"'\n{tail}");
            assert!(!has_content_hint(source.as_bytes(), true), "{source:?}");
        }
    }

    #[test]
    fn fsharp_sampled_annotation_paths_do_not_accept_unquoted_keywords() {
        for keyword in ["if", "let", "match", "null", "protected", "trait"] {
            for path in [keyword.to_owned(), format!("Example.{keyword}")] {
                let source = format!("module Example\nlet value = (1\n: {path})\n");
                assert!(!has_content_hint(source.as_bytes(), true), "{source:?}");
            }
        }
        for path in ["Item", "Example.Item", "constructor", "method", "object"] {
            let source = format!("module Example\nlet value = (1\n: {path})\n");
            assert!(has_content_hint(source.as_bytes(), true), "{source:?}");
        }
    }

    #[test]
    fn fsharp_quote_character_separators_retain_token_adjacency() {
        for literal in ["'\"'", "'\\\"'"] {
            let source = format!(
                "module Example\nlet values = [| ' '; {literal}; '\\'' |]\n\
                 let value = (1\n: int)\n"
            );
            assert!(has_content_hint(source.as_bytes(), true), "{source:?}");
            let mixed = format!("{source}: square dup ;\n");
            assert!(!has_content_hint(mixed.as_bytes(), true), "{mixed:?}");
        }
    }

    #[test]
    fn fsharp_quote_characters_retain_declaration_evidence() {
        for literal in ["'\"'", "'\\\"'"] {
            for suffix in ["", "B"] {
                for newline in ["\n", "\r\n"] {
                    let source = format!(
                        "\u{feff}module Example{newline}let quote = {literal}{suffix}{newline}"
                    );
                    assert!(has_content_hint(source.as_bytes(), false), "{source:?}");
                    let sampled = format!("{source}let unfinished = ");
                    assert!(has_content_hint(sampled.as_bytes(), true), "{sampled:?}");
                }
            }
        }
    }

    #[test]
    fn fsharp_quote_character_masking_keeps_literal_and_apostrophe_boundaries() {
        for (literal, masked) in [("'\"'", "\" \""), ("'\\\"'", "\"  \"")] {
            let source = format!(
                "let quote = {literal}\nlet identity (value: 'T) = value\n\
                 let value' = 'a'\nlet escaped = [ '\\''; '\\\\' ]\n"
            );
            let expected = source.replacen(literal, masked, 1);
            assert_eq!(
                mask_quoted_and_comment_contents(source.as_bytes(), false),
                Some(expected.into_bytes()),
            );
        }
        for literal in ["'\"", "'\\\""] {
            let source = format!("module Example\nlet quote = {literal}");
            assert!(!has_content_hint(source.as_bytes(), false), "{source:?}");
            assert!(!has_content_hint(source.as_bytes(), true), "{source:?}");
        }
        let mut sampled = b"module Example\nlet quote = '\"'\n".to_vec();
        sampled.extend_from_slice(&[0xe2, 0x82]);
        assert!(has_content_hint(&sampled, true));
        assert!(!has_content_hint(&sampled, false));
    }

    #[test]
    fn fsharp_quote_characters_do_not_hide_visible_forth_definitions() {
        for literal in ["'\"'", "'\\\"'"] {
            for trailing in [": square dup * ;\n", ": square"] {
                let source = format!("module Example\nlet quote = {literal}\n{trailing}");
                assert!(!has_content_hint(source.as_bytes(), true), "{source:?}");
                if trailing.ends_with('\n') {
                    assert!(!has_content_hint(source.as_bytes(), false), "{source:?}");
                }
            }
        }
        for source in [
            "// module Example\n// let quote = '\"'\n",
            "(* module Example\nlet quote = '\\\"' *)\n",
            "\"module Example\nlet quote = '\\\"'\"\n",
        ] {
            assert!(!has_content_hint(source.as_bytes(), false), "{source:?}");
        }
    }

    #[test]
    fn fsharp_module_accessibility_retains_declaration_evidence() {
        for accessibility in ["private", "internal", "public"] {
            for recursive in ["", "rec "] {
                for header in [
                    format!("module {accessibility} {recursive}Example.Core"),
                    format!("module {accessibility} {recursive}Example ="),
                    format!("module\t{accessibility}\t{recursive}Example.Core"),
                ] {
                    for newline in ["\n", "\r\n"] {
                        let source = format!(
                            "\u{feff}// declaration evidence{newline}\
                             (* outer (* nested *) comment *){newline}\
                             {header}{newline}    let value = 1{newline}"
                        );
                        assert!(has_content_hint(source.as_bytes(), false), "{source:?}");
                    }
                }
            }
        }
    }

    #[test]
    fn fsharp_finite_prologue_preserves_complete_header_and_declaration_evidence() {
        for prologue in [
            "open Example.Core\n",
            "[<Example.Marker>]\n",
            "[<AutoOpen>]\n",
            "#nowarn \"9\"\n",
            "#nowarn \"0042\"\nopen Example.Core\n[<Example.Marker>]\n",
            "open Example.Core\n\n// no source evidence\n[<Example.Marker>]\n",
        ] {
            for header in ["module internal Example", "namespace Example.Core"] {
                for newline in ["\n", "\r\n"] {
                    let source =
                        format!("\u{feff}// initial comment\n{prologue}{header}\nlet value = 1\n")
                            .replace('\n', newline);
                    assert!(has_content_hint(source.as_bytes(), false), "{source:?}");
                    let mut grown = source.into_bytes();
                    grown.extend_from_slice(b"// unrelated tail ");
                    let bound = super::super::MAX_CLASSIFICATION_BYTES;
                    grown.resize(bound + 1, b'x');
                    assert!(has_content_hint(&grown[..bound], true), "{prologue:?}");
                }
            }
        }
    }

    #[test]
    fn fsharp_conditional_scope_headers_preserve_scope_and_declaration_evidence() {
        for (first, alternate) in [
            ("namespace Example.First", "namespace Example.Second"),
            ("module Example.First", "module internal Example.Second"),
            ("module Example.First", "namespace Example.Second"),
        ] {
            for newline in ["\n", "\r\n"] {
                for prologue in ["", "open Example.Support\n#nowarn \"42\"\n"] {
                    let source = format!(
                        "\u{feff}// source ownership\n{prologue}\
                         (* complete outer (* nested *) comment *)\n\
                         \t#if ROUTE_MODE // complete condition\n\
                         \n(* first arm *) {first} // first scope\n\
                         \t#else (* complete separator *)\n\
                         // alternative arm\n{alternate} (* complete scope comment *)\n\
                         #endif // complete block\n\
                         let value = 1\n"
                    )
                    .replace('\n', newline);
                    assert!(has_content_hint(source.as_bytes(), false), "{source:?}");
                    assert!(has_content_hint(source.as_bytes(), true), "{source:?}");
                }
            }
        }
    }

    #[test]
    fn fsharp_conditional_scope_headers_require_complete_authored_branches() {
        let complete =
            "#if ROUTE_MODE\nnamespace Example.First\n#else\nnamespace Example.Second\n#endif\n";
        for source in [
            "#if\nnamespace Example.First\n#else\nnamespace Example.Second\n#endif\nlet value = 1\n",
            "#if true\nnamespace Example.First\n#else\nnamespace Example.Second\n#endif\nlet value = 1\n",
            "#if false\nnamespace Example.First\n#else\nnamespace Example.Second\n#endif\nlet value = 1\n",
            "#if !ROUTE_MODE\nnamespace Example.First\n#else\nnamespace Example.Second\n#endif\nlet value = 1\n",
            "#if (ROUTE_MODE)\nnamespace Example.First\n#else\nnamespace Example.Second\n#endif\nlet value = 1\n",
            "#if ROUTE_MODE && OTHER_MODE\nnamespace Example.First\n#else\nnamespace Example.Second\n#endif\nlet value = 1\n",
            "#if ROUTE_MODE || OTHER_MODE\nnamespace Example.First\n#else\nnamespace Example.Second\n#endif\nlet value = 1\n",
            "#if ROUTE_MODE OTHER_MODE\nnamespace Example.First\n#else\nnamespace Example.Second\n#endif\nlet value = 1\n",
            "#if RO(* hidden *)UTE_MODE\nnamespace Example.First\n#else\nnamespace Example.Second\n#endif\nlet value = 1\n",
            "#if \"ROUTE_MODE\"\nnamespace Example.First\n#else\nnamespace Example.Second\n#endif\nlet value = 1\n",
            "#if ROUTE_MODE\nmodule \"hidden\" Example.First\n#else\nnamespace Example.Second\n#endif\nlet value = 1\n",
            "#if ROUTE_MODE\nmodule (* hidden *) Example.First\n#else\nnamespace Example.Second\n#endif\nlet value = 1\n",
            "#if ROUTE_MODE\nnamespace Example.First\n#else\nnamespace Ex(* hidden *)ample.Second\n#endif\nlet value = 1\n",
            "#if ROUTE_MODE\nnamespace Example.First.\n#else\nnamespace Example.Second\n#endif\nlet value = 1\n",
            "#if ROUTE_MODE\nnamespace Example.First\n#else\nmodule internal\n#endif\nlet value = 1\n",
            "#if ROUTE_MODE\nnamespace Example.First\n#endif\nlet value = 1\n",
            "#if ROUTE_MODE\nnamespace Example.First\n#else\n#endif\nlet value = 1\n",
            "#if ROUTE_MODE\n#else\nnamespace Example.Second\n#endif\nlet value = 1\n",
            "#if ROUTE_MODE\nnamespace Example.First\n#elif OTHER_MODE\nnamespace Example.Second\n#endif\nlet value = 1\n",
            "#if ROUTE_MODE\n#if OTHER_MODE\nnamespace Example.First\n#else\nnamespace Example.Second\n#endif\n#else\nnamespace Example.Third\n#endif\nlet value = 1\n",
            "#if ROUTE_MODE\nnamespace Example.First\nlet branch_value = 1\n#else\nnamespace Example.Second\n#endif\nlet value = 1\n",
            "#if ROUTE_MODE\nnamespace Example.First\n#else extra\nnamespace Example.Second\n#endif\nlet value = 1\n",
            "#if ROUTE_MODE\nnamespace Example.First\n#else\nnamespace Example.Second\n#endif extra\nlet value = 1\n",
            "#if ROUTE_MODE\n#else\n#endif\nlet value: int = 1\n",
        ] {
            for truncated in [false, true] {
                assert!(
                    !has_content_hint(source.as_bytes(), truncated),
                    "{source:?}"
                );
            }
        }
        for tail in [
            "",
            "let value =\n",
            "// let value = 1\n",
            "\"let value = 1\"\n",
        ] {
            let source = format!("{complete}{tail}");
            assert!(!has_content_hint(source.as_bytes(), false), "{source:?}");
        }
    }

    #[test]
    fn fsharp_sampled_conditional_scope_headers_do_not_complete_partial_proofs() {
        for newline in ["\n", "\r\n"] {
            let block = "#if ROUTE_MODE\nnamespace Example.First\n#else\nnamespace Example.Second\n#endif\n".replace('\n', newline);
            let prefix = format!("{block}let value = 1{newline}");
            let sample = full_sample_with_tail(&prefix, "// bounded tail", newline);
            assert!(has_content_hint(&sample, true));
            for tail in [
                "#if ROUTE_MODE",
                "#if ROUTE_MODE\nnamespace Example.First",
                "#if ROUTE_MODE\nnamespace Example.First\n#else",
                "#if ROUTE_MODE\nnamespace Example.First\n#else\nnamespace Example.Second",
                "#if ROUTE_MODE\nnamespace Example.First\n#else\nnamespace Example.Second\n#endif",
                "#if ROUTE_MODE\nnamespace Example.First\n#else\nnamespace Example.Second\n#endif\nlet value = 1",
                "#if ROUTE_MODE\nnamespace Example.First\n#else\nnamespace Example.Second\n#endif\nlet value =\n    va",
            ] {
                let tail = tail.replace('\n', newline);
                let sample = full_sample_with_tail("", &tail, newline);
                assert!(!has_content_hint(&sample, true), "{tail:?}");
            }
            let mut grown = prefix.into_bytes();
            grown.extend_from_slice(b"// grown unrelated tail ");
            let bound = super::super::MAX_CLASSIFICATION_BYTES;
            grown.resize(bound + 1, b'x');
            assert!(has_content_hint(&grown[..bound], true));
        }
    }

    #[test]
    fn fsharp_conditional_scope_headers_preserve_whole_sample_forth_veto() {
        let block = "#if ROUTE_MODE\nmodule Example.First\n#else\nmodule Example.Second\n#endif\n";
        for conflict in [": square dup * ;\n", ": square dup + ;\n"] {
            for position in 0..3 {
                let source = match position {
                    0 => format!("{conflict}{block}let value = 1\n"),
                    1 => format!("{block}{conflict}let value = 1\n"),
                    _ => format!("{block}let value = 1\n{conflict}"),
                };
                for truncated in [false, true] {
                    assert!(
                        !has_content_hint(source.as_bytes(), truncated),
                        "{source:?}"
                    );
                }
            }
        }
        let prefix = format!("{block}let value = 1\n");
        for tail in [": square", ": square dup + ;", ": square\n"] {
            let sample = full_sample_with_tail(&prefix, tail, "\n");
            assert!(!has_content_hint(&sample, true), "{tail:?}");
        }
    }

    #[test]
    fn fsharp_finite_prologue_rejects_unproved_lines_and_literal_splices() {
        for prologue in [
            "open\n",
            "open Example.\n",
            "open Example..Core\n",
            "open type Example.Core\n",
            "open Example Core\n",
            "open ``Example.Core``\n",
            "open Ex(* hidden *)ample.Core\n",
            "[<Example.Marker(1)>]\n",
            "[<Example.Marker; Other.Marker>]\n",
            "[<assembly: Example.Marker>]\n",
            "[<Example..Marker>]\n",
            "[<Example.Marker]\n",
            "[<``Example.Marker``>]\n",
            "[<Ex(* hidden *)ample.Marker>]\n",
            "#nowarn \"\"\n",
            "#nowarn \"a42\"\n",
            "#nowarn \"42\" \"43\"\n",
            "#nowarn \"\\x34\\x32\"\n",
            "#nowarn @\"42\"\n",
            "#nowarn \"４２\"\n",
            "#nowarn \"42\" (* extra *)\n",
            "#if INTERACTIVE\n",
            "#else\n",
            "#endif\n",
            "#light \"on\"\n",
            "#load \"other.fsx\"\n",
            "let earlier = 1\n",
            "foreign words\n",
        ] {
            let source = format!("{prologue}module Example\nlet value = 1\n");
            for truncated in [false, true] {
                assert!(
                    !has_content_hint(source.as_bytes(), truncated),
                    "{source:?}"
                );
            }
        }
    }

    #[test]
    fn fsharp_finite_prologue_cannot_replace_anchors_or_hide_visible_forth() {
        for prologue in [
            "open Example.Core\n",
            "[<Example.Marker>]\n",
            "#nowarn \"42\"\n",
        ] {
            for tail in [
                "",
                "let value =\n",
                "module Example\n",
                "module Example\nlet value =\n",
                "module Example\n// let value = 1\n",
                "module Example\nlet value = 1\n: square dup * ;\n",
                "module Example\nlet value = 1\n: square",
                "module Example\nlet value = 1",
            ] {
                let source = format!("{prologue}{tail}");
                assert!(!has_content_hint(source.as_bytes(), true), "{source:?}");
            }
        }
        for source in [
            "open Example.Core\nmodule Example",
            "[<Example.Marker",
            "#nowarn \"42",
            "#nowarn 42\nmodule Example\nlet value = 1\n",
            "\"open Example.Core\"\nmodule Example\nlet value = 1\n",
            "\"[<Example.Marker>]\"\nmodule Example\nlet value = 1\n",
            "\"#nowarn \\\"42\\\"\"\nmodule Example\nlet value = 1\n",
        ] {
            assert!(!has_content_hint(source.as_bytes(), true), "{source:?}");
        }
    }

    #[test]
    fn fsharp_module_accessibility_does_not_complete_invalid_headers_or_evidence() {
        for header in [
            "module private",
            "module internal rec",
            "module private private",
            "module public private Example",
            "module rec internal Example",
            "module public rec rec Example",
            "module protected Example",
            "namespace private Example",
            "namespace internal rec Example",
            "namespace public Example",
        ] {
            let source = format!("{header}\nlet value = 1\n");
            assert!(!has_content_hint(source.as_bytes(), false), "{source:?}");
        }
        for source in [
            "module internal Example\n",
            "module internal Example\nlet value =\n",
            "// module internal Example\n// let value = 1\n",
            "(* module internal Example\nlet value = 1 *)\n",
            "\"module internal Example\nlet value = 1\"\n",
            "module internal Example\nlet value = 1\n: square dup * ;\n",
        ] {
            assert!(!has_content_hint(source.as_bytes(), false), "{source:?}");
        }
        assert!(!has_content_hint(
            b"module internal Example\nlet value = 1",
            true,
        ));
        let mut outside = vec![b' '; super::super::MAX_CLASSIFICATION_BYTES];
        outside.extend_from_slice(b"module internal Example\nlet value = 1\n");
        assert!(!has_content_hint(
            &outside[..super::super::MAX_CLASSIFICATION_BYTES],
            true,
        ));
    }

    #[test]
    fn fsharp_content_accepts_complete_indented_multiline_declarations() {
        for declaration in [
            "let value =\n    1",
            "let add left right =\n    left + right",
            "let rec private visit value =\n    value",
            "type Entry =\n    { Count: int }",
            "type Choice =\n    | Empty\n    | Item of int",
            "module Inner =\n    let value =\n        1",
            "let value = (* a complete comment *)\n\n    // no declaration evidence\n    (* outer (* nested *) comment *)\n    1",
            "let text =\n    \"module Ghost\\nlet ghost = 1\"",
            "let ``quoted value`` =\n    1",
        ] {
            let source = format!("module Example\n{declaration}");
            for ending in ["", "\n"] {
                for newline in ["\n", "\r\n"] {
                    let source = format!("{source}{ending}").replace('\n', newline);
                    assert!(has_content_hint(source.as_bytes(), false), "{source:?}");
                }
            }
        }
    }

    #[test]
    fn fsharp_multiline_evidence_does_not_complete_missing_or_conflicting_rhs() {
        for declaration in [
            "let value =",
            "let value =\n    // no expression",
            "let value =\n    (* only a comment *)",
            "let value =\n1",
            "    let value =\n    1",
            "    let value =\n  1",
            "\tlet value =\n\t\t1",
            "let value =\n\t1",
            "let value =\n \t1",
            "let value =\n    = 1",
            "let value =\n    == 1",
            "let value =\n    \"unfinished",
            "let value =\n    (* unfinished",
            "let ``unfinished =\n    1",
            "let 1value =\n    1",
            "let value =\n    : square dup * ;",
            "type Entry =\n    : square dup * ;",
            "let value =\n    (* no expression *)\nmodule Other",
        ] {
            let source = format!("module Example\n{declaration}\n");
            assert!(!has_content_hint(source.as_bytes(), false), "{source:?}");
        }
        assert!(!has_content_hint(b"let value =\n    1\n", false));
        assert!(!has_content_hint(
            b"module Example\nlet value =\n    1",
            true
        ));
        assert!(!has_content_hint(
            b"module Example\nlet value =\n    \"unfinished\n",
            true,
        ));
        assert!(!has_content_hint(
            b"module Example\nlet value =\n    (* unfinished\n",
            true,
        ));
        assert!(!has_content_hint(
            b"module Example\nlet value =\n    ``unfinished\n",
            true,
        ));
        let mut outside = vec![b' '; super::super::MAX_CLASSIFICATION_BYTES];
        outside.extend_from_slice(b"module Example\nlet value =\n    1\n");
        assert!(!has_content_hint(
            &outside[..super::super::MAX_CLASSIFICATION_BYTES],
            true,
        ));
    }

    #[test]
    fn fsharp_content_keeps_complete_evidence_before_a_bounded_tail() {
        let bound = super::super::MAX_CLASSIFICATION_BYTES;
        for (tail, remainder) in [
            ("let next = 123", "4\n"),
            ("let next = \"unfinished", "\"\n"),
            ("(* unfinished", " *)\n"),
            ("let ``unfinished", "`` = 2\n"),
        ] {
            let mut source = b"module Example\nlet value = 1\n".to_vec();
            source.resize(bound - tail.len(), b' ');
            source.extend_from_slice(tail.as_bytes());
            source.extend_from_slice(remainder.as_bytes());
            assert!(
                has_content_hint(&source[..bound], source.len() > bound),
                "{tail:?}"
            );
        }
        let mut source = b"module Example\nlet value = 1\n".to_vec();
        source.resize(bound - 1, b' ');
        source.extend_from_slice("\u{03bb}\n".as_bytes());
        assert!(has_content_hint(&source[..bound], true));
        assert!(!has_content_hint(&source[..bound], false));
    }

    #[test]
    fn fsharp_bounded_tail_cannot_create_an_anchor_or_hide_visible_forth() {
        let bound = super::super::MAX_CLASSIFICATION_BYTES;
        for tail in [
            "let value = 12",
            "let value = \"unfinished",
            "let ``unfinished",
        ] {
            let mut source = b"module Example\n".to_vec();
            source.resize(bound - tail.len(), b' ');
            source.extend_from_slice(tail.as_bytes());
            assert!(!has_content_hint(&source, true), "{tail:?}");
        }
        for tail in [b": square".as_slice(), b": square dup * ;", b"\xff\n"] {
            let mut source = b"module Example\nlet value = 1\n".to_vec();
            source.extend_from_slice(tail);
            source.resize(bound, b' ');
            assert!(!has_content_hint(&source, true), "{tail:?}");
        }
    }

    #[test]
    fn fsharp_content_requires_an_opening_header_and_real_declaration_anchor() {
        for source in [
            "module Example.Core\nlet value = 1\n",
            "namespace Example.Core\ntype Entry = { Count: int }\n",
            "module Example =\n    let add x y = x + y\n",
            "\u{feff}(* outer (* nested *) comment *)\r\nmodule Example\r\nlet value = @\"a \"\"quoted\"\" literal\"\r\n",
            "module Example\nlet text = \"\"\"module Ghost\nlet ghost = 1\n: square dup * ;\n\"\"\"\n",
            "module Example\nlet value : int = 1 ;\n",
            "module Example\ntype Entry = { Pair : int * int ; Name : string }\n",
            "module Example\nlet value = [ 1 ] : int list ;\n",
            "module Example\nlet value = (1 : int) ;\n",
            "module Example\nlet value = 1\ntype Contract =\n    abstract Pair : int * int\n",
            "module Example\nlet ``value : square dup * ;`` = 1\n",
            "module ``Example : square dup * ;``\nlet value = 1\n",
            "module Example\nlet text = \": square dup * ;\"\n",
            "module Example\nlet text = @\": square \"\"dup\"\" * ;\"\n",
            "module Example\nlet value = 1 (* : square dup * ; *)\n",
        ] {
            assert!(has_content_hint(source.as_bytes(), false), "{source:?}");
        }
    }

    #[test]
    fn fsharp_content_rejects_forth_literal_comments_and_mixed_or_incomplete_evidence() {
        for source in [
            "",
            "module Example\n",
            "let value = 1\n",
            ": square dup * ;\n",
            ": square dup * ;\n// module Example\n// let value = 1\n",
            "(* module Example\nlet value = 1 *)\n",
            "\"module Example\nlet value = 1\"\n",
            "s\" module Example\nlet value = 1\"\n",
            ".\" module Example\nlet value = 1\"\n",
            "@\"module Example\nlet value = 1\"\n",
            "\"\"\"module Example\nlet value = 1\n\"\"\"\n",
            "module Example\nlet value = 1\n: square\n  dup * ;\n",
            "module Example\nlet value = 1 : square dup * ;\n",
            "module Example\nlet value = 1\n0 [if] : square dup * ; [then]\n",
            "module Example\n// let value = 1\n",
            "module Example\nlet value =\n",
            "module Example\nlet value == 1\n",
            "module Example\nlet value = \"unfinished\nlet other = 1\n",
            "module Example\nlet value = 1\n(* unfinished",
            "module Example\nlet value = 1\n``unfinished : square dup * ;",
        ] {
            assert!(!has_content_hint(source.as_bytes(), false), "{source:?}");
        }
    }

    #[test]
    fn fsharp_content_never_uses_incomplete_utf8_or_lexical_boundary_fragments() {
        let bound = super::super::MAX_CLASSIFICATION_BYTES;
        for tail in ["λ", "\"unfinished", "(* unfinished", "let value ="] {
            let mut source = b"module Example\n".to_vec();
            source.resize(bound - 1, b' ');
            source.extend_from_slice(tail.as_bytes());
            assert!(!has_content_hint(&source[..bound], false));
        }
        let mut source = b"module Example\nlet value = 1\n".to_vec();
        source.resize(bound - 1, b' ');
        source.extend_from_slice("λ".as_bytes());
        assert!(!has_content_hint(&source[..bound], false));
    }
}
