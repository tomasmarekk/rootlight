//! Shared parser-independent structural fact classification.

use std::{borrow::Cow, cmp::Ordering};

use rootlight_ir::EntityKind;

use crate::{SyntaxFact, SyntaxFactKind};

/// Returns the grammar-reviewed base entity kind for one structural syntax fact.
///
/// File and project lowering share this base classifier and refine direct
/// type members with [`structural_type_member_kind`]. Rust `impl` ownership
/// additionally requires the caller's language-specific scope evidence.
#[must_use]
pub fn structural_entity_kind(fact: &SyntaxFact) -> Option<EntityKind> {
    let label = fact.syntax_kind().as_str();
    match fact.kind() {
        SyntaxFactKind::Module if label == "vba.class.module" => Some(EntityKind::Class),
        SyntaxFactKind::Module => Some(EntityKind::Module),
        SyntaxFactKind::Declaration
            if matches!(
                label,
                "javascript.import_binding.declaration"
                    | "typescript.import_binding.declaration"
                    | "javascript.type_import_binding.declaration"
                    | "typescript.type_import_binding.declaration"
                    | "groovy.import_alias.declaration"
            ) =>
        {
            Some(EntityKind::Import)
        }
        SyntaxFactKind::Declaration if label == "markdown.section.declaration" => {
            Some(EntityKind::DocumentSection)
        }
        SyntaxFactKind::Declaration if label == "make.target.declaration" => {
            Some(EntityKind::BuildTarget)
        }
        SyntaxFactKind::Declaration
            if matches!(
                label,
                "typescript.type_parameter.declaration"
                    | "typescript.mapped_type_parameter.declaration"
                    | "typescript.infer_parameter.declaration"
                    | "objective_c.type_parameter.declaration"
                    | "groovy.type_parameter.declaration"
            ) =>
        {
            Some(EntityKind::TypeParameter)
        }
        SyntaxFactKind::Declaration if label == "markdown.link_definition.declaration" => {
            Some(EntityKind::LinkDefinition)
        }
        SyntaxFactKind::Declaration
            if matches!(
                label,
                "dart.extension.declaration"
                    | "powershell.hashtable.declaration"
                    | "scala.object.declaration"
                    | "scala.package.declaration"
                    | "scala.package_object.declaration"
            ) =>
        {
            Some(EntityKind::Namespace)
        }
        SyntaxFactKind::Declaration if label == "scala.enum_value.declaration" => {
            Some(EntityKind::Constant)
        }
        SyntaxFactKind::Declaration
            if matches!(
                label,
                "solidity.event.declaration" | "vba.event.declaration"
            ) =>
        {
            Some(EntityKind::Event)
        }
        SyntaxFactKind::Declaration if label == "solidity.error.declaration" => {
            Some(EntityKind::ErrorDeclaration)
        }
        SyntaxFactKind::Declaration if label == "solidity.modifier.declaration" => {
            Some(EntityKind::Modifier)
        }
        SyntaxFactKind::Declaration if label == "solidity.enum_value.declaration" => {
            Some(EntityKind::Constant)
        }
        SyntaxFactKind::Declaration
            if matches!(
                label,
                "sql.table.declaration"
                    | "sql.view.declaration"
                    | "sql.materialized_view.declaration"
                    | "sql.index.declaration"
                    | "sql.sequence.declaration"
                    | "sql.trigger.declaration"
                    | "sql.database.declaration"
                    | "sql.role.declaration"
                    | "sql.extension.declaration"
                    | "sql.type.declaration"
            ) =>
        {
            Some(EntityKind::DatabaseObject)
        }
        SyntaxFactKind::Declaration if label == "sql.schema.declaration" => {
            Some(EntityKind::Namespace)
        }
        SyntaxFactKind::Declaration if label == "sql.column.declaration" => Some(EntityKind::Field),
        SyntaxFactKind::Declaration
            if matches!(
                label,
                "html.element.declaration" | "astro.element.declaration"
            ) =>
        {
            Some(EntityKind::MarkupElement)
        }
        SyntaxFactKind::Declaration
            if matches!(
                label,
                "html.attribute.declaration" | "astro.attribute.declaration"
            ) =>
        {
            Some(EntityKind::MarkupAttribute)
        }
        SyntaxFactKind::Declaration
            if matches!(
                label,
                "toml.table.declaration" | "toml.table_array_element.declaration"
            ) =>
        {
            Some(EntityKind::Namespace)
        }
        SyntaxFactKind::Declaration
            if matches!(
                label,
                "css.style_rule.declaration" | "scss.style_rule.declaration"
            ) =>
        {
            Some(EntityKind::StyleRule)
        }
        SyntaxFactKind::Declaration
            if matches!(
                label,
                "css.keyframes.declaration" | "scss.keyframes.declaration"
            ) =>
        {
            Some(EntityKind::Keyframes)
        }
        SyntaxFactKind::Declaration if label == "css.property.declaration" => {
            Some(EntityKind::Property)
        }
        SyntaxFactKind::Declaration if label == "yaml.anchor.declaration" => {
            Some(EntityKind::Variable)
        }
        SyntaxFactKind::Declaration if label == "scss.variable.declaration" => {
            Some(EntityKind::Variable)
        }
        SyntaxFactKind::Declaration if label == "scss.mixin.declaration" => {
            Some(EntityKind::Function)
        }
        SyntaxFactKind::Declaration
            if matches!(
                label,
                "json.property.declaration"
                    | "groovy.property.declaration"
                    | "matlab.property.declaration"
                    | "objective_c.property.declaration"
                    | "powershell.property.declaration"
                    | "powershell.dynamic_property.declaration"
                    | "toml.property.declaration"
                    | "yaml.property.declaration"
                    | "vba.get_property.declaration"
                    | "vba.let_property.declaration"
                    | "vba.set_property.declaration"
                    | "vba.conditional_property.declaration"
            ) =>
        {
            Some(EntityKind::Property)
        }
        SyntaxFactKind::Declaration
            if matches!(
                label,
                "swift.protocol.declaration"
                    | "objective_c.protocol.declaration"
                    | "objective_c.forward_protocol.declaration"
            ) =>
        {
            Some(EntityKind::Protocol)
        }
        // Actors are reference types; the syntax label retains their concurrency distinction.
        SyntaxFactKind::Declaration if label == "swift.actor.declaration" => {
            Some(EntityKind::Class)
        }
        SyntaxFactKind::Declaration if label == "swift.property.declaration" => {
            Some(EntityKind::Property)
        }
        SyntaxFactKind::Declaration
            if matches!(
                label,
                "ruby.namespace.declaration" | "perl.package.declaration"
            ) =>
        {
            Some(EntityKind::Namespace)
        }
        SyntaxFactKind::Declaration if label == "java.annotation.declaration" => {
            Some(EntityKind::Interface)
        }
        SyntaxFactKind::Declaration if label == "java.annotation_element.declaration" => {
            Some(EntityKind::Method)
        }
        SyntaxFactKind::Declaration if label == "elm.port.declaration" => {
            Some(EntityKind::Function)
        }
        SyntaxFactKind::Declaration
            if matches!(
                label,
                "prolog.predicate.declaration" | "prolog.nonterminal.declaration"
            ) =>
        {
            Some(EntityKind::Function)
        }
        SyntaxFactKind::Declaration if label.contains("constructor") => {
            Some(EntityKind::Constructor)
        }
        SyntaxFactKind::Declaration if label.contains("record") => Some(EntityKind::Struct),
        SyntaxFactKind::Declaration if label.contains("method") => Some(EntityKind::Method),
        SyntaxFactKind::Declaration if label.contains("function") => Some(EntityKind::Function),
        SyntaxFactKind::Declaration if label.contains("class") => Some(EntityKind::Class),
        SyntaxFactKind::Declaration if label.contains("struct") => Some(EntityKind::Struct),
        SyntaxFactKind::Declaration if label == "vba.enum_constant.declaration" => {
            Some(EntityKind::Constant)
        }
        SyntaxFactKind::Declaration if label.contains("enum") => Some(EntityKind::Enum),
        SyntaxFactKind::Declaration if label.contains("trait") => Some(EntityKind::Trait),
        SyntaxFactKind::Declaration if label.contains("interface") => Some(EntityKind::Interface),
        SyntaxFactKind::Declaration
            if label.contains("type_alias")
                || label.contains("type_item")
                || label.contains("type.declaration") =>
        {
            Some(EntityKind::TypeAlias)
        }
        SyntaxFactKind::Declaration
            if label.contains("constant")
                || label.contains("const_item")
                || label.contains("const.declaration") =>
        {
            Some(EntityKind::Constant)
        }
        SyntaxFactKind::Declaration
            if label.contains("static_item") || label.contains("static.declaration") =>
        {
            Some(EntityKind::Variable)
        }
        SyntaxFactKind::Declaration if label.contains("field") => Some(EntityKind::Field),
        SyntaxFactKind::Declaration if label.contains("parameter") => Some(EntityKind::Parameter),
        SyntaxFactKind::Declaration if label.contains("variable") => Some(EntityKind::Variable),
        _ => None,
    }
}

/// Refines a structural kind when the caller has proven a direct type owner.
///
/// Both file and project lowering use this rule. External VBA declarations
/// describe imported procedures, not methods implemented by an instance.
#[must_use]
pub fn structural_type_member_kind(fact: &SyntaxFact, kind: EntityKind) -> EntityKind {
    match (kind, fact.syntax_kind().as_str()) {
        (
            EntityKind::Function,
            "vba.external_function.declaration" | "vba.external_sub_function.declaration",
        ) => kind,
        (EntityKind::Function, _) => EntityKind::Method,
        (EntityKind::Variable, "vba.variable.declaration") => EntityKind::Field,
        _ => kind,
    }
}

/// Refines a base structural kind using only the captured declaration source.
///
/// The Go grammar represents aliases, structs, and interfaces with one
/// `type_spec` node. Its declaration text is therefore the only reviewed local
/// evidence that can distinguish those kinds without repository execution.
#[must_use]
pub fn structural_entity_kind_from_source(
    fact: &SyntaxFact,
    declaration: &str,
) -> Option<EntityKind> {
    let kind = structural_entity_kind(fact)?;
    if fact.syntax_kind().as_str() != "go.type.declaration" {
        return Some(kind);
    }
    let header = declaration.trim_start();
    if header.contains("interface") {
        Some(EntityKind::Interface)
    } else if header.contains("struct") {
        Some(EntityKind::Struct)
    } else {
        Some(kind)
    }
}

/// Returns the bounded name accepted from a reviewed structural capture.
///
/// Project analyzers use the same boundary when preserving structural
/// declarations so grammar-valid scoped names cannot disappear during
/// semantic refinement.
#[must_use]
pub fn structural_captured_name(text: &str, maximum_bytes: usize) -> Option<&str> {
    let candidate = text.trim();
    (!candidate.is_empty()
        && candidate.len() <= maximum_bytes
        && candidate.chars().all(|character| {
            !character.is_control()
                && !character.is_whitespace()
                && !matches!(character, '/' | '\\' | '(' | ')' | '{' | '}' | '[' | ']')
        }))
    .then_some(candidate)
}

/// Returns a bounded canonical name from a grammar-reviewed language capture.
///
/// Lua member captures may contain formatting or comments between qualifiers.
/// Their canonical identity removes only that trivia; the caller must preserve
/// the original source span. Ruby additionally admits its bracket and division
/// method operators, including a static singleton receiver. CSS preserves raw
/// grammar-reviewed selectors and identifiers: whitespace, escapes and Unicode
/// normalization can change their meaning. This boundary bounds those captures;
/// it does not validate arbitrary CSS text. JSON keys retain a canonical quoted
/// JSON string, so empty keys and controls cannot collide with ordinary names.
/// Equivalent escapes have the same identity; unpaired UTF-16 units remain
/// escaped rather than becoming replacement characters. TOML key paths quote
/// each decoded segment independently: `a.b` differs from `"a.b"`, while bare,
/// basic and literal spellings of the same segment agree. TOML rejects non-scalar
/// escapes; this validates key syntax, not table ownership or document semantics.
/// YAML untagged flow scalars use YAML 1.2 Core resolution and a type-prefixed
/// canonical identity. Plain, single-quoted and double-quoted strings fold lines
/// and decode their own escapes; numeric identities do not round through floats.
/// This scalar boundary does not resolve document directives, tags, anchors,
/// aliases, block scalars or collection keys; those require document context.
/// HTML preserves grammar-reviewed source names verbatim, including foreign
/// markup spelling; it does not perform browser case or namespace adjustments.
/// SQL removes only trivia between qualified name components, retaining case,
/// delimiters and escape spelling until a dialect-aware resolver is available.
/// R decodes backticks and quoted assignment targets into UTF-8 names without
/// case folding or Unicode normalization. Invalid or non-UTF-8 escapes remain
/// unavailable; canonical spelling does not infer runtime binding equivalence.
/// Scala removes enclosing identifier backticks without changing their contents;
/// grammar-reviewed symbolic names retain their exact spelling, including `/`.
/// Dart removes trivia between qualified name components and additionally retains
/// its grammar-reviewed division and bracket operators, without resolving receivers.
/// PowerShell preserves written names, including braced variables, without runtime
/// scope expansion, escape evaluation or case-insensitive binding equivalence.
/// Nix retains reviewed static attribute paths exactly, including quotes and trivia;
/// canonical source identity is not evaluated attribute-name equivalence.
/// Markdown preserves authored heading and label text; rendered text, reference
/// case folding and generated fragment identifiers require separate interpretation.
/// Make retains reviewed static relative paths in targets and prerequisites;
/// expansions and escaped names remain outside that borrowed-name contract.
/// ECMAScript identifiers decode Unicode escapes without Unicode normalization;
/// other grammar-reviewed names keep the shared borrowed-name contract.
/// Groovy constant member strings decode their own delimiters and ordered escapes.
/// Empty, control-bearing, edge-whitespace and quote-prefixed names retain an
/// injective JSON-quoted identity, including unpaired UTF-16 units. Interpolated
/// names are unavailable rather than evaluated; source spans retain all quoting.
/// Other languages retain
/// the shared borrowed-name contract. Source and canonical output must both fit
/// `maximum_bytes`. Invalid names, excess bytes or allocation failure return `None`.
#[must_use]
pub fn structural_captured_name_for_language<'a>(
    language: &str,
    text: &'a str,
    maximum_bytes: usize,
) -> Option<Cow<'a, str>> {
    if matches!(language, "javascript" | "typescript") {
        crate::canonical_ecmascript_identifier(text, maximum_bytes)
            .or_else(|| structural_captured_name(text, maximum_bytes).map(Cow::Borrowed))
    } else if language == "groovy" {
        crate::groovy_names::canonical_groovy_name(text, maximum_bytes)
    } else if language == "scala" {
        let candidate = text.trim();
        if candidate.is_empty() || candidate.len() > maximum_bytes {
            return None;
        }
        if let Some(quoted) = candidate
            .strip_prefix('`')
            .and_then(|name| name.strip_suffix('`'))
        {
            return (!quoted.is_empty() && !quoted.chars().any(|ch| ch.is_control() || ch == '`'))
                .then_some(Cow::Borrowed(quoted));
        }
        candidate
            .chars()
            .all(|ch| {
                !ch.is_control()
                    && !ch.is_whitespace()
                    && !matches!(ch, '(' | ')' | '{' | '}' | '[' | ']' | '`')
            })
            .then_some(Cow::Borrowed(candidate))
    } else if language == "sql" {
        crate::sql_names::canonical_sql_name(text, maximum_bytes)
    } else if language == "json" {
        crate::json_names::canonical_json_key(text, maximum_bytes)
    } else if language == "toml" {
        crate::toml_names::canonical_toml_key_path(text, maximum_bytes)
    } else if language == "yaml" {
        crate::yaml_names::canonical_flow_key(text, maximum_bytes).map(Cow::Owned)
    } else if language == "r" {
        crate::r_names::canonical_r_name(text, maximum_bytes)
    } else if matches!(language, "css" | "html" | "powershell" | "markdown" | "nix") {
        (!text.is_empty() && text.len() <= maximum_bytes && !text.contains('\0'))
            .then_some(Cow::Borrowed(text))
    } else if language == "lua" {
        crate::lua_names::canonical_lua_name(text, maximum_bytes)
    } else if language == "dart" {
        crate::dart_names::canonical_dart_name(text, maximum_bytes)
    } else if language == "ruby" {
        let candidate = text.trim();
        let operator = candidate
            .rsplit_once('.')
            .map_or(candidate, |(_, name)| name);
        let static_receiver = candidate.rsplit_once('.').is_none_or(|(receiver, _)| {
            structural_captured_name(receiver, maximum_bytes) == Some(receiver)
        });
        if candidate.len() <= maximum_bytes
            && static_receiver
            && matches!(operator, "/" | "[]" | "[]=")
        {
            Some(Cow::Borrowed(candidate))
        } else {
            structural_captured_name(candidate, maximum_bytes).map(Cow::Borrowed)
        }
    } else {
        structural_captured_name(text, maximum_bytes).map(Cow::Borrowed)
    }
}

/// Interprets a reviewed name capture with its structural role and language.
///
/// YAML anchor definitions and alias references are serialization names, not
/// scalar values: `true`, `11` and quote characters retain their exact spelling.
/// Their captures exclude the `&` or `*` indicator. No trimming, escape decoding
/// or Unicode normalization occurs; malformed or oversized names return `None`.
/// Groovy qualified identifiers remove inter-component spaces and block comments
/// without changing component spelling. VBA identifiers remove enclosing brackets
/// or a trailing type character, retaining original case and source token spans.
/// This spelling projection does not establish case-insensitive binding identity.
/// SCSS style-rule selectors retain authored lists and whitespace without
/// claiming that interpolation has been expanded.
/// Other roles use
/// [`structural_captured_name_for_language`]. The caller retains
/// the original source span and validates its association with its declaration.
#[must_use]
pub fn structural_captured_name_for_fact<'a>(
    language: &str,
    fact: &SyntaxFact,
    text: &'a str,
    maximum_bytes: usize,
) -> Option<Cow<'a, str>> {
    if language == "make"
        && matches!(
            fact.syntax_kind().as_str(),
            "make.name.definition" | "make.target.reference"
        )
    {
        let bytes = text.as_bytes();
        (bytes.len() <= maximum_bytes
            && bytes
                .first()
                .is_some_and(|byte| byte.is_ascii_alphabetic() || matches!(byte, b'_' | b'.'))
            && bytes.iter().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b'/')
            }))
        .then_some(Cow::Borrowed(text))
    } else if language == "scss"
        && matches!(
            fact.syntax_kind().as_str(),
            "scss.selector.definition" | "scss.selector.signature"
        )
    {
        (!text.is_empty() && text.len() <= maximum_bytes && !text.contains('\0'))
            .then_some(Cow::Borrowed(text))
    } else if language == "prolog" && fact.syntax_kind().as_str() == "prolog.predicate.definition" {
        crate::prolog_names::canonical_predicate_head(text, maximum_bytes).map(Cow::Owned)
    } else if language == "prolog" && fact.syntax_kind().as_str() == "prolog.nonterminal.definition"
    {
        crate::prolog_names::canonical_nonterminal_head(text, maximum_bytes).map(Cow::Owned)
    } else if matches!(language, "vba" | "visual-basic-classic")
        && fact.syntax_kind().as_str() == "vba.module_name.definition"
    {
        (text.len() <= maximum_bytes)
            .then(|| vba_module_name(text))
            .flatten()
            .map(Cow::Borrowed)
    } else if matches!(language, "vba" | "visual-basic-classic")
        && matches!(
            fact.syntax_kind().as_str(),
            "vba.identifier.definition"
                | "vba.identifier.reference"
                | "vba.value_identifier.reference"
                | "vba.let_identifier.reference"
                | "vba.set_identifier.reference"
                | "vba.invoked_get_identifier.reference"
                | "vba.invoked_let_identifier.reference"
                | "vba.invoked_set_identifier.reference"
                | "vba.default_constant_identifier.reference"
                | "vba.direct_type_identifier.reference"
                | "vba.type_identifier.reference"
                | "vba.invoked_identifier.reference"
                | "vba.self_member.reference"
                | "vba.self_invoked_member.reference"
                | "vba.self_get_member.reference"
                | "vba.self_let_member.reference"
                | "vba.self_set_member.reference"
                | "vba.self_invoked_get_member.reference"
                | "vba.self_invoked_let_member.reference"
                | "vba.self_invoked_set_member.reference"
        )
    {
        if text.is_empty() || text.len() > maximum_bytes {
            return None;
        }
        if let Some(name) = text
            .strip_prefix('[')
            .and_then(|name| name.strip_suffix(']'))
        {
            return (!name.is_empty()
                && !name
                    .chars()
                    .any(|character| character.is_control() || matches!(character, '[' | ']')))
            .then_some(Cow::Borrowed(name));
        }
        let name = text
            .strip_suffix(['$', '%', '&', '#', '@', '^', '!'])
            .unwrap_or(text);
        structural_captured_name(name, maximum_bytes).map(Cow::Borrowed)
    } else if language == "yaml"
        && fact.kind() == SyntaxFactKind::Occurrence
        && matches!(
            fact.syntax_kind().as_str(),
            "yaml.anchor.definition" | "yaml.alias.reference"
        )
    {
        crate::yaml_names::anchor_name(text, maximum_bytes).map(Cow::Borrowed)
    } else if language == "groovy"
        && matches!(
            fact.syntax_kind().as_str(),
            "groovy.qualified_identifier.definition"
                | "groovy.qualified_type.reference"
                | "groovy.extends_type.reference"
                | "groovy.implements_type.reference"
                | "groovy.type_import.reference"
                | "groovy.package_star_import.reference"
                | "groovy.static_import.reference"
                | "groovy.static_star_import.reference"
        )
    {
        crate::groovy_names::canonical_qualified_name(text, maximum_bytes)
    } else {
        structural_captured_name_for_language(language, text, maximum_bytes)
    }
}

/// Returns the unchanged Latin name from a quoted VBA module-header value.
///
/// MS-VBAL section 4.2 limits module names to 31 characters. This projection
/// accepts ASCII letters followed by ASCII letters, digits or underscores;
/// other code-page-dependent names remain unproven, not normalized or replaced.
/// Invalid, unquoted or unsupported values return `None`. Callers preserve the
/// original quoted source span and account for unavailable identities separately.
#[must_use]
pub fn vba_module_name(text: &str) -> Option<&str> {
    let name = text.strip_prefix('"')?.strip_suffix('"')?;
    (name.len() <= 31
        && name.as_bytes().first().is_some_and(u8::is_ascii_alphabetic)
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_'))
    .then_some(name)
}

/// Returns readable display text for a canonical structural identity.
///
/// JSON scalar keys omit their enclosing quotes and decode quote/backslash
/// escapes. Empty, boundary-whitespace, control-bearing and unpaired-UTF-16 keys
/// retain the canonical quoted identity. TOML uses the same display rule for a
/// single segment; multi-segment paths stay quoted to preserve boundaries.
/// YAML string identities use the same readable-string rule; other scalar types
/// keep their type prefix so a boolean or number cannot masquerade as a string.
/// PowerShell literal keys decode quoting and escapes for readable display only;
/// their canonical identity remains the exact written spelling. Unreadable or
/// interpolated strings retain that spelling instead of inventing a value.
/// Nix static keys decode escapes for display, and paths omit trivia while
/// retaining quoted nonidentifier segments. A dotted single key stays quoted;
/// ambiguous or expanding display text retains the original written name.
/// Groovy's quoted exceptional identities use the same readable-string rule;
/// controls, empty names and unpaired UTF-16 units remain visibly escaped.
/// Other languages are unchanged. The input must already be a
/// bounded canonical name from [`structural_captured_name_for_language`]; display
/// text is no longer than that input and must never replace the durable identity.
#[must_use]
pub fn structural_display_name_for_language<'a>(
    language: &str,
    canonical: &'a str,
) -> Cow<'a, str> {
    if language == "powershell" {
        crate::powershell_names::display_key(canonical).unwrap_or(Cow::Borrowed(canonical))
    } else if language == "nix" {
        crate::nix_names::display_path(canonical).unwrap_or(Cow::Borrowed(canonical))
    } else if matches!(language, "json" | "toml")
        || (language == "groovy" && canonical.starts_with('"'))
    {
        crate::json_names::display_json_key(canonical).unwrap_or(Cow::Borrowed(canonical))
    } else if language == "yaml" {
        canonical
            .strip_prefix("str:")
            .and_then(crate::json_names::display_json_key)
            .unwrap_or(Cow::Borrowed(canonical))
    } else {
        Cow::Borrowed(canonical)
    }
}

/// Orders syntax facts exactly as structural declaration association consumes them.
///
/// Whole-project analyzers must use this order before resolving nearest
/// declarations. Parser-local identifiers are intentionally excluded because
/// their assignment order is not part of the structural identity contract.
#[must_use]
pub fn structural_syntax_fact_order(left: &SyntaxFact, right: &SyntaxFact) -> Ordering {
    (
        left.depth(),
        left.span().start_byte(),
        left.span().end_byte(),
        syntax_fact_kind_tag(left.kind()),
        left.syntax_kind().as_str(),
    )
        .cmp(&(
            right.depth(),
            right.span().start_byte(),
            right.span().end_byte(),
            syntax_fact_kind_tag(right.kind()),
            right.syntax_kind().as_str(),
        ))
}

const fn syntax_fact_kind_tag(kind: SyntaxFactKind) -> u8 {
    match kind {
        SyntaxFactKind::Root => 1,
        SyntaxFactKind::Module => 2,
        SyntaxFactKind::Declaration => 3,
        SyntaxFactKind::Signature => 4,
        SyntaxFactKind::Import => 5,
        SyntaxFactKind::Scope => 6,
        SyntaxFactKind::Occurrence => 7,
        SyntaxFactKind::Comment => 8,
        SyntaxFactKind::StringLiteral => 9,
        SyntaxFactKind::EmbeddedRegion => 10,
        SyntaxFactKind::ErrorRecovery => 11,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vba_class_and_type_member_kinds_use_reviewed_fact_labels() {
        use crate::SyntaxKindLabel;
        use rootlight_ids::FileId;
        use rootlight_ir::SourceSpan;

        for (label, fact_kind, base, member) in [
            (
                "vba.class.module",
                SyntaxFactKind::Module,
                EntityKind::Class,
                EntityKind::Class,
            ),
            (
                "vba.file.module",
                SyntaxFactKind::Module,
                EntityKind::Module,
                EntityKind::Module,
            ),
            (
                "vba.variable.declaration",
                SyntaxFactKind::Declaration,
                EntityKind::Variable,
                EntityKind::Field,
            ),
            (
                "vba.function.declaration",
                SyntaxFactKind::Declaration,
                EntityKind::Function,
                EntityKind::Method,
            ),
            (
                "vba.sub_function.declaration",
                SyntaxFactKind::Declaration,
                EntityKind::Function,
                EntityKind::Method,
            ),
            (
                "vba.external_function.declaration",
                SyntaxFactKind::Declaration,
                EntityKind::Function,
                EntityKind::Function,
            ),
            (
                "vba.external_sub_function.declaration",
                SyntaxFactKind::Declaration,
                EntityKind::Function,
                EntityKind::Function,
            ),
            (
                "vba.get_property.declaration",
                SyntaxFactKind::Declaration,
                EntityKind::Property,
                EntityKind::Property,
            ),
            (
                "vba.constant.declaration",
                SyntaxFactKind::Declaration,
                EntityKind::Constant,
                EntityKind::Constant,
            ),
            (
                "javascript.variable.declaration",
                SyntaxFactKind::Declaration,
                EntityKind::Variable,
                EntityKind::Variable,
            ),
            (
                "scss.style_rule.declaration",
                SyntaxFactKind::Declaration,
                EntityKind::StyleRule,
                EntityKind::StyleRule,
            ),
            (
                "scss.keyframes.declaration",
                SyntaxFactKind::Declaration,
                EntityKind::Keyframes,
                EntityKind::Keyframes,
            ),
        ] {
            let fact = SyntaxFact::new(
                1,
                None,
                fact_kind,
                SourceSpan::new(FileId::from_bytes([3; 20]), 0, 1).unwrap(),
                0,
                SyntaxKindLabel::new(label).unwrap(),
            );
            assert_eq!(structural_entity_kind(&fact), Some(base), "{label}");
            assert_eq!(structural_type_member_kind(&fact, base), member, "{label}");
        }
    }

    #[test]
    fn elm_port_declarations_retain_function_identity() {
        use crate::SyntaxKindLabel;
        use rootlight_ids::FileId;
        use rootlight_ir::SourceSpan;

        let fact = SyntaxFact::new(
            1,
            None,
            SyntaxFactKind::Declaration,
            SourceSpan::new(FileId::from_bytes([3; 20]), 0, 1).unwrap(),
            0,
            SyntaxKindLabel::new("elm.port.declaration").unwrap(),
        );
        assert_eq!(structural_entity_kind(&fact), Some(EntityKind::Function));
    }

    #[test]
    fn scss_selector_capture_preserves_lists_without_loosening_identifiers() {
        use crate::SyntaxKindLabel;
        use rootlight_ids::FileId;
        use rootlight_ir::SourceSpan;

        let span = SourceSpan::new(FileId::from_bytes([3; 20]), 0, 1).unwrap();
        let selector = SyntaxFact::new(
            1,
            None,
            SyntaxFactKind::Occurrence,
            span,
            0,
            SyntaxKindLabel::new("scss.selector.definition").unwrap(),
        );
        let identifier = SyntaxFact::new(
            2,
            None,
            SyntaxFactKind::Occurrence,
            span,
            0,
            SyntaxKindLabel::new("scss.identifier.definition").unwrap(),
        );
        assert_eq!(
            structural_captured_name_for_fact("scss", &selector, ".card, .tile", 32),
            Some(Cow::Borrowed(".card, .tile"))
        );
        assert!(structural_captured_name_for_fact("scss", &selector, ".card, .tile", 4).is_none());
        assert!(structural_captured_name_for_fact("scss", &selector, "a\0b", 32).is_none());
        assert!(
            structural_captured_name_for_fact("scss", &identifier, ".card, .tile", 32).is_none()
        );
    }

    #[test]
    fn make_static_paths_keep_authored_identity_without_relaxing_other_names() {
        use crate::SyntaxKindLabel;
        use rootlight_ids::FileId;
        use rootlight_ir::SourceSpan;

        let span = SourceSpan::new(FileId::from_bytes([3; 20]), 0, 1).unwrap();
        for label in ["make.name.definition", "make.target.reference"] {
            let fact = SyntaxFact::new(
                1,
                None,
                SyntaxFactKind::Occurrence,
                span,
                0,
                SyntaxKindLabel::new(label).unwrap(),
            );
            assert_eq!(
                structural_captured_name_for_fact("make", &fact, "src/main.c", 32).as_deref(),
                Some("src/main.c")
            );
            for name in ["/root", "src/$(NAME)", "src\\name", "src name"] {
                assert!(structural_captured_name_for_fact("make", &fact, name, 32).is_none());
            }
            assert!(structural_captured_name_for_fact("make", &fact, "src/main.c", 4).is_none());
            assert!(structural_captured_name_for_fact("rust", &fact, "src/main.c", 32).is_none());
        }
    }

    #[test]
    fn vba_module_names_preserve_case_and_enforce_latin_identifier_bounds() {
        for name in ["A", "Metrics_2", "abcdefghijklmnopqrstuvwxyzABCDE"] {
            let quoted = format!("\"{name}\"");
            assert_eq!(vba_module_name(&quoted), Some(name));
        }
        for text in [
            "",
            "Metrics",
            "\"\"",
            "\"_Name\"",
            "\"9Name\"",
            "\"Name$\"",
            "\"A.B\"",
            "\"A B\"",
            "\"A\"\"B\"",
            "\"Δelta\"",
            "\"abcdefghijklmnopqrstuvwxyzABCDEF\"",
        ] {
            assert_eq!(vba_module_name(text), None, "{text}");
        }
    }

    #[test]
    fn nix_display_names_decode_static_paths_without_changing_authored_identity() {
        for (written, display) in [
            (r#""identity""#, "identity"),
            (r#""\identity""#, "identity"),
            (r#""λ😀""#, "λ😀"),
            (r#""two words""#, "two words"),
            (r#""\u0078""#, "u0078"),
            (r#"a."\b""#, "a.b"),
            ("a /* path trivia */ . b", "a.b"),
            ("a # path trivia\r\n . b", "a.b"),
            (r#""a.b""#, r#""a.b""#),
            (r#""a.b" . c"#, r#""a.b".c"#),
            (r#"a . "b c""#, r#"a."b c""#),
            (r#""""#, r#""""#),
            (r#""\n""#, r#""\n""#),
        ] {
            assert_eq!(
                structural_display_name_for_language("nix", written),
                display,
                "{written}"
            );
            assert_eq!(
                structural_captured_name_for_language("nix", written, written.len()).as_deref(),
                Some(written)
            );
            assert!(display.len() <= written.len());
        }
    }

    #[test]
    fn scala_names_preserve_operators_and_bounded_backtick_identity() {
        for (source, canonical) in [
            ("name", "name"),
            ("`name`", "name"),
            ("`odd name`", "odd name"),
            ("/", "/"),
            ("\\", "\\"),
            ("λ", "λ"),
        ] {
            assert_eq!(
                structural_captured_name_for_language("scala", source, source.len()).as_deref(),
                Some(canonical)
            );
            assert!(
                structural_captured_name_for_language("scala", source, source.len() - 1).is_none()
            );
        }
        for invalid in [
            "",
            "``",
            "`name",
            "name`",
            "`a`b`",
            "two names",
            "call()",
            "`line\nname`",
        ] {
            assert!(
                structural_captured_name_for_language("scala", invalid, 64).is_none(),
                "{invalid:?}"
            );
        }
    }

    #[test]
    fn json_display_names_decode_only_readable_canonical_keys() {
        for (raw, display) in [
            (r#""name""#, "name"),
            (r#""\u006eame""#, "name"),
            (r#""\ud83e\udd80""#, "🦀"),
            (r#""a\"b""#, "a\"b"),
            (r#""a\\b""#, "a\\b"),
            (r#""""#, r#""""#),
            (r#""\u0000""#, r#""\u0000""#),
            (r#""\ud800""#, r#""\ud800""#),
            (r#"" ""#, r#"" ""#),
            (r#"" name ""#, r#"" name ""#),
            ("\"\u{2003}name\"", "\"\u{2003}name\""),
        ] {
            let canonical =
                structural_captured_name_for_language("json", raw, 64).expect("valid key");
            assert_eq!(
                structural_display_name_for_language("json", &canonical),
                display
            );
            assert_eq!(
                structural_display_name_for_language("rust", &canonical),
                canonical
            );
            assert!(display.len() <= canonical.len());
        }
    }

    #[test]
    fn ruby_operator_names_are_bounded_and_do_not_relax_other_languages() {
        for name in ["/", "[]", "[]=", "self.[]", "Store./"] {
            assert_eq!(
                structural_captured_name_for_language("ruby", name, 64),
                Some(Cow::Borrowed(name))
            );
            assert_eq!(
                structural_captured_name_for_language("ruby", name, name.len() - 1),
                None
            );
            assert_eq!(
                structural_captured_name_for_language("rust", name, 64),
                None
            );
        }
        for name in [
            "./",
            "factory().[]",
            "self.[value]",
            "[ ]",
            "Store .[]",
            "self.[]\0",
        ] {
            assert_eq!(
                structural_captured_name_for_language("ruby", name, 64),
                None,
                "{name:?}"
            );
        }
    }
    #[test]
    fn r_name_spellings_decode_without_changing_other_language_names() {
        for (source, expected) in [
            ("identity", "identity"),
            ("`identity`", "identity"),
            ("\"identity\"", "identity"),
            ("'identity'", "identity"),
            (r"`\x69dentity`", "identity"),
            (r#""\151dentity""#, "identity"),
            (r#""\u0069dentity""#, "identity"),
            (r#"r"(identity)""#, "identity"),
            ("`with spaces`", "with spaces"),
            (r#""\u03bb""#, "λ"),
            (r#""\xce\xbb""#, "λ"),
        ] {
            assert_eq!(
                structural_captured_name_for_language("r", source, 128).as_deref(),
                Some(expected),
                "{source}"
            );
            assert!(structural_captured_name_for_language("r", source, source.len() - 1).is_none());
        }
        assert_eq!(
            structural_captured_name_for_language("html", "`identity`", 128).as_deref(),
            Some("`identity`")
        );
    }

    #[test]
    fn powershell_written_names_are_bounded_without_runtime_normalization() {
        for source in [
            "Read-Entry",
            "script:Read-Entry",
            "$Name",
            "${name with space}",
            "${雪}",
        ] {
            assert_eq!(
                structural_captured_name_for_language("powershell", source, source.len())
                    .as_deref(),
                Some(source)
            );
            assert!(
                structural_captured_name_for_language("powershell", source, source.len() - 1)
                    .is_none()
            );
        }
        for source in ["", "name\0tail"] {
            assert!(structural_captured_name_for_language("powershell", source, 128).is_none());
        }
        assert!(structural_captured_name_for_language("rust", "${name with space}", 128).is_none());
    }

    #[test]
    fn markdown_names_preserve_authored_spelling_with_exact_byte_limits() {
        for source in [
            "Storage *rules*",
            "[Guide]",
            "[guide]",
            "雪 &amp; `code`",
            "First\r\nsecond",
        ] {
            assert_eq!(
                structural_captured_name_for_language("markdown", source, source.len()),
                Some(Cow::Borrowed(source))
            );
            assert!(
                structural_captured_name_for_language("markdown", source, source.len() - 1)
                    .is_none()
            );
        }
        for source in ["", "head\0tail"] {
            assert!(structural_captured_name_for_language("markdown", source, 128).is_none());
        }
        assert!(structural_captured_name_for_language("rust", "Storage *rules*", 128).is_none());
    }

    #[test]
    fn r_invalid_name_escapes_do_not_invent_a_canonical_symbol() {
        for source in [
            r"`\u0061`",
            r#""\x61\u0062""#,
            r#""\0""#,
            r#""\400""#,
            r#""\q""#,
            r#""\xff""#,
            r#""\uD800""#,
            "\"\"",
            "`unclosed",
            "`a` trailing",
        ] {
            assert!(
                structural_captured_name_for_language("r", source, 128).is_none(),
                "{source}"
            );
        }
    }
    #[test]
    fn groovy_quoted_names_preserve_constant_identity() {
        for (source, expected) in [
            ("'with space'", "with space"),
            ("\"double space\"", "double space"),
            ("'''triple space'''", "triple space"),
            ("\"\"\"triple double\"\"\"", "triple double"),
            ("/slash name/", "slash name"),
            ("$/dollar name/$", "dollar name"),
            (r"'uni\u0063ode'", "unicode"),
            (r"'oct\141l'", "octal"),
            (r"'space\sname'", "space name"),
            (r"'quote\'name'", "quote'name"),
            (r"'slash\\name'", r"slash\name"),
            (r"'surrogate\uD83D\uDE80'", "surrogate🚀"),
            (r"'line\nname'", r#""line\u000aname""#),
            (r"'lone\uD800'", r#""lone\ud800""#),
            ("''", "\"\""),
            (r"'${name}'", "${name}"),
            (r#""dollar\$name""#, "dollar$name"),
            (r"/literal\n/", r"literal\n"),
            (r"/uni\u0061/", "unia"),
            (r"$/uni\u0061/$", "unia"),
            ("$/literal$$name/$", "literal$name"),
        ] {
            assert_eq!(
                structural_captured_name_for_language("groovy", source, 128).as_deref(),
                Some(expected),
                "{source}"
            );
        }
    }
}
