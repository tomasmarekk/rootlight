//! Only dialect-certified schema labels establish model and property entities.
//! Name captures keep the exact unquoted JSON source extent; canonical decoding
//! does not change that extent or infer declarations from generic JSON members.

use std::borrow::Cow;

use rootlight_ir::EntityKind;

use crate::{SyntaxFact, SyntaxFactKind};

pub(super) fn entity_kind(fact: &SyntaxFact) -> Option<EntityKind> {
    match (fact.kind(), fact.syntax_kind().as_str()) {
        (SyntaxFactKind::Module, "json_schema.file.module") => Some(EntityKind::Module),
        (SyntaxFactKind::Declaration, "json_schema.object_schema.declaration") => {
            Some(EntityKind::Struct)
        }
        (SyntaxFactKind::Declaration, "json_schema.named_schema.declaration") => {
            Some(EntityKind::TypeAlias)
        }
        (SyntaxFactKind::Declaration, "json_schema.field.declaration") => Some(EntityKind::Field),
        _ => None,
    }
}

pub(super) fn captured_body(text: &str, maximum: usize) -> Option<Cow<'_, str>> {
    let quoted_length = text.len().checked_add(2)?;
    if text.len() > maximum {
        return None;
    }
    let mut quoted = String::new();
    quoted.try_reserve(quoted_length).ok()?;
    quoted.push('"');
    quoted.push_str(text);
    quoted.push('"');
    let canonical = crate::json_names::canonical_json_key(&quoted, quoted_length)?;
    // Empty/control-only names have no reviewed readable schema name yet.
    // Their declaration-name gap preserves source instead of inventing an alias.
    crate::json_names::display_json_key(canonical.as_ref())
        .map(|decoded| Cow::Owned(decoded.into_owned()))
}

pub(super) fn captured_name_for_fact<'a>(
    fact: &SyntaxFact,
    text: &'a str,
    maximum: usize,
) -> Option<Cow<'a, str>> {
    if fact.kind() != SyntaxFactKind::Occurrence {
        return None;
    }
    match fact.syntax_kind().as_str() {
        "json_schema.name.definition" => captured_body(text, maximum),
        "json_schema.local_ref.reference"
        | "json_schema.unresolved_ref.reference"
        | "json_schema.unsupported_resource_ref.reference" => {
            (text.len() <= maximum).then_some(Cow::Borrowed(text))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_names_decode_source_escapes_without_quoting_ordinary_schema_names() {
        for (body, expected) in [
            ("test", "test"),
            ("data", "data"),
            (r"caf\u00e9", "café"),
            (r"a\/b~c", "a/b~c"),
            (r#"a\"b"#, "a\"b"),
        ] {
            assert_eq!(captured_body(body, 64).as_deref(), Some(expected));
        }
        assert_eq!(captured_body("test", 3), None);
        assert_eq!(captured_body("test", 4).as_deref(), Some("test"));
        for body in ["", r"\u0000", r"\ud800", r"\z", " leading", "trailing "] {
            assert!(captured_body(body, 64).is_none(), "{body}");
        }
    }
}
