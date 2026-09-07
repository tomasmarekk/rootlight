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
        SyntaxFactKind::Declaration if label == "css.style_rule.declaration" => {
            Some(EntityKind::StyleRule)
        }
        SyntaxFactKind::Declaration if label == "css.keyframes.declaration" => {
            Some(EntityKind::Keyframes)
        }
        SyntaxFactKind::Declaration if label == "css.property.declaration" => {
            Some(EntityKind::Property)
        }
        SyntaxFactKind::Declaration if label == "json.property.declaration" => {
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
/// escaped rather than becoming replacement characters. Other languages retain
/// the shared borrowed-name contract. Source and canonical output must both fit
/// `maximum_bytes`. Invalid names, excess bytes or allocation failure return `None`.
#[must_use]
pub fn structural_captured_name_for_language<'a>(
    language: &str,
    text: &'a str,
    maximum_bytes: usize,
) -> Option<Cow<'a, str>> {
    if language == "json" {
        crate::json_names::canonical_json_key(text, maximum_bytes)
    } else if language == "css" {
        (!text.is_empty() && text.len() <= maximum_bytes && !text.contains('\0'))
            .then_some(Cow::Borrowed(text))
    } else if language == "lua" {
        crate::lua_names::canonical_lua_name(text, maximum_bytes)
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

/// Returns readable display text for a canonical structural identity.
///
/// JSON scalar keys omit their enclosing quotes and decode quote/backslash
/// escapes. Empty, boundary-whitespace, control-bearing and unpaired-UTF-16 keys
/// retain the canonical quoted identity. Other languages are unchanged. The input must already be a
/// bounded canonical name from [`structural_captured_name_for_language`]; display
/// text is no longer than that input and must never replace the durable identity.
#[must_use]
pub fn structural_display_name_for_language<'a>(
    language: &str,
    canonical: &'a str,
) -> Cow<'a, str> {
    if language == "json" {
        crate::json_names::display_json_key(canonical).unwrap_or(Cow::Borrowed(canonical))
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
}
