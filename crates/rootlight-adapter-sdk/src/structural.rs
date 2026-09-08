//! Shared parser-independent structural fact classification.

use std::{borrow::Cow, cmp::Ordering};

use rootlight_ir::EntityKind;

use crate::{SyntaxFact, SyntaxFactKind};

/// Returns the grammar-reviewed base entity kind for one structural syntax fact.
///
/// Contextual lowering may refine a function to a method when its parent is a
/// type or a Rust `impl`. Whole-project analysis uses the same base classifier
/// before applying that identical contextual refinement.
#[must_use]
pub fn structural_entity_kind(fact: &SyntaxFact) -> Option<EntityKind> {
    let label = fact.syntax_kind().as_str();
    match fact.kind() {
        SyntaxFactKind::Module => Some(EntityKind::Module),
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
        SyntaxFactKind::Declaration if label == "solidity.event.declaration" => {
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
        SyntaxFactKind::Declaration if label == "html.element.declaration" => {
            Some(EntityKind::MarkupElement)
        }
        SyntaxFactKind::Declaration if label == "html.attribute.declaration" => {
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
        SyntaxFactKind::Declaration if label == "css.style_rule.declaration" => {
            Some(EntityKind::StyleRule)
        }
        SyntaxFactKind::Declaration if label == "css.keyframes.declaration" => {
            Some(EntityKind::Keyframes)
        }
        SyntaxFactKind::Declaration if label == "css.property.declaration" => {
            Some(EntityKind::Property)
        }
        SyntaxFactKind::Declaration if label == "yaml.anchor.declaration" => {
            Some(EntityKind::Variable)
        }
        SyntaxFactKind::Declaration
            if matches!(
                label,
                "json.property.declaration"
                    | "powershell.property.declaration"
                    | "powershell.dynamic_property.declaration"
                    | "toml.property.declaration"
                    | "yaml.property.declaration"
            ) =>
        {
            Some(EntityKind::Property)
        }
        SyntaxFactKind::Declaration if label == "swift.protocol.declaration" => {
            Some(EntityKind::Protocol)
        }
        // Actors are reference types; the syntax label retains their concurrency distinction.
        SyntaxFactKind::Declaration if label == "swift.actor.declaration" => {
            Some(EntityKind::Class)
        }
        SyntaxFactKind::Declaration if label == "swift.property.declaration" => {
            Some(EntityKind::Property)
        }
        SyntaxFactKind::Declaration if label == "ruby.namespace.declaration" => {
            Some(EntityKind::Namespace)
        }
        SyntaxFactKind::Declaration if label == "java.annotation.declaration" => {
            Some(EntityKind::Interface)
        }
        SyntaxFactKind::Declaration if label == "java.annotation_element.declaration" => {
            Some(EntityKind::Method)
        }
        SyntaxFactKind::Declaration if label.contains("constructor") => {
            Some(EntityKind::Constructor)
        }
        SyntaxFactKind::Declaration if label.contains("record") => Some(EntityKind::Struct),
        SyntaxFactKind::Declaration if label.contains("method") => Some(EntityKind::Method),
        SyntaxFactKind::Declaration if label.contains("function") => Some(EntityKind::Function),
        SyntaxFactKind::Declaration if label.contains("class") => Some(EntityKind::Class),
        SyntaxFactKind::Declaration if label.contains("struct") => Some(EntityKind::Struct),
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
/// Other languages retain
/// the shared borrowed-name contract. Source and canonical output must both fit
/// `maximum_bytes`. Invalid names, excess bytes or allocation failure return `None`.
#[must_use]
pub fn structural_captured_name_for_language<'a>(
    language: &str,
    text: &'a str,
    maximum_bytes: usize,
) -> Option<Cow<'a, str>> {
    if language == "scala" {
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
    } else if matches!(language, "css" | "html" | "powershell") {
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
/// Other roles use [`structural_captured_name_for_language`]. The caller retains
/// the original source span and validates its association with its declaration.
#[must_use]
pub fn structural_captured_name_for_fact<'a>(
    language: &str,
    fact: &SyntaxFact,
    text: &'a str,
    maximum_bytes: usize,
) -> Option<Cow<'a, str>> {
    if language == "yaml"
        && fact.kind() == SyntaxFactKind::Occurrence
        && matches!(
            fact.syntax_kind().as_str(),
            "yaml.anchor.definition" | "yaml.alias.reference"
        )
    {
        crate::yaml_names::anchor_name(text, maximum_bytes).map(Cow::Borrowed)
    } else {
        structural_captured_name_for_language(language, text, maximum_bytes)
    }
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
    } else if matches!(language, "json" | "toml") {
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
}
