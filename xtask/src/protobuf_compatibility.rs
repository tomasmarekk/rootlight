//! Semantic compatibility checks for frozen protobuf descriptor sets.
//!
//! Historical descriptors are required subsets of the current contract: new
//! declarations may be added, but released names, numbers, types, and presence
//! semantics cannot be removed or repurposed.

use std::collections::BTreeMap;

use prost_types::{
    DescriptorProto, EnumDescriptorProto, FieldDescriptorProto, FileDescriptorSet,
    OneofDescriptorProto,
};

pub(crate) fn require_compatible(
    historical: &FileDescriptorSet,
    current: &FileDescriptorSet,
) -> Result<(), CompatibilityError> {
    let historical_files = index_named(&historical.file, |file| file.name.as_deref());
    let current_files = index_named(&current.file, |file| file.name.as_deref());
    for (file_name, historical_file) in historical_files {
        let current_file = current_files
            .get(file_name)
            .ok_or_else(|| CompatibilityError::removed("file", file_name))?;
        if historical_file.package != current_file.package
            || historical_file.syntax != current_file.syntax
        {
            return Err(CompatibilityError::changed("file", file_name));
        }
        compare_messages(
            historical_file.package.as_deref().unwrap_or_default(),
            &historical_file.message_type,
            &current_file.message_type,
        )?;
        compare_enums(
            historical_file.package.as_deref().unwrap_or_default(),
            &historical_file.enum_type,
            &current_file.enum_type,
        )?;
    }
    Ok(())
}

fn compare_messages(
    scope: &str,
    historical: &[DescriptorProto],
    current: &[DescriptorProto],
) -> Result<(), CompatibilityError> {
    let current_by_name = index_named(current, |message| message.name.as_deref());
    for historical_message in historical {
        let name = historical_message
            .name
            .as_deref()
            .ok_or_else(|| CompatibilityError::missing_name("message", scope))?;
        let qualified = qualify(scope, name);
        let current_message = current_by_name
            .get(name)
            .ok_or_else(|| CompatibilityError::removed("message", &qualified))?;
        compare_oneofs(
            &qualified,
            &historical_message.oneof_decl,
            &current_message.oneof_decl,
        )?;
        compare_fields(&qualified, historical_message, current_message)?;
        compare_messages(
            &qualified,
            &historical_message.nested_type,
            &current_message.nested_type,
        )?;
        compare_enums(
            &qualified,
            &historical_message.enum_type,
            &current_message.enum_type,
        )?;
    }
    Ok(())
}

fn compare_fields(
    scope: &str,
    historical: &DescriptorProto,
    current: &DescriptorProto,
) -> Result<(), CompatibilityError> {
    let current_by_name = index_named(&current.field, |field| field.name.as_deref());
    let current_by_number: BTreeMap<i32, &FieldDescriptorProto> = current
        .field
        .iter()
        .filter_map(|field| field.number.map(|number| (number, field)))
        .collect();

    for historical_field in &historical.field {
        let name = historical_field
            .name
            .as_deref()
            .ok_or_else(|| CompatibilityError::missing_name("field", scope))?;
        let number = historical_field
            .number
            .ok_or_else(|| CompatibilityError::missing_number("field", &qualify(scope, name)))?;
        let qualified = qualify(scope, name);
        let Some(current_field) = current_by_name.get(name) else {
            if let Some(reused) = current_by_number.get(&number) {
                return Err(CompatibilityError::number_reused(
                    &qualified,
                    number,
                    reused.name.as_deref().unwrap_or("<unnamed>"),
                ));
            }
            return Err(CompatibilityError::removed("field", &qualified));
        };
        if current_field.number != Some(number) {
            return Err(CompatibilityError::renumbered(
                &qualified,
                number,
                current_field.number,
            ));
        }
        if historical_field.label != current_field.label
            || historical_field.r#type != current_field.r#type
            || historical_field.type_name != current_field.type_name
            || historical_field.oneof_index != current_field.oneof_index
            || historical_field.proto3_optional != current_field.proto3_optional
            || packed(historical_field) != packed(current_field)
        {
            return Err(CompatibilityError::changed("field", &qualified));
        }
    }
    Ok(())
}

fn compare_oneofs(
    scope: &str,
    historical: &[OneofDescriptorProto],
    current: &[OneofDescriptorProto],
) -> Result<(), CompatibilityError> {
    for (index, historical_oneof) in historical.iter().enumerate() {
        let name = historical_oneof
            .name
            .as_deref()
            .ok_or_else(|| CompatibilityError::missing_name("oneof", scope))?;
        let Some(current_oneof) = current.get(index) else {
            return Err(CompatibilityError::removed("oneof", &qualify(scope, name)));
        };
        if current_oneof.name.as_deref() != Some(name) {
            return Err(CompatibilityError::changed("oneof", &qualify(scope, name)));
        }
    }
    Ok(())
}

fn compare_enums(
    scope: &str,
    historical: &[EnumDescriptorProto],
    current: &[EnumDescriptorProto],
) -> Result<(), CompatibilityError> {
    let current_by_name = index_named(current, |enumeration| enumeration.name.as_deref());
    for historical_enum in historical {
        let name = historical_enum
            .name
            .as_deref()
            .ok_or_else(|| CompatibilityError::missing_name("enum", scope))?;
        let qualified = qualify(scope, name);
        let current_enum = current_by_name
            .get(name)
            .ok_or_else(|| CompatibilityError::removed("enum", &qualified))?;
        let current_values_by_name =
            index_named(&current_enum.value, |value| value.name.as_deref());
        let current_values_by_number: BTreeMap<i32, &str> = current_enum
            .value
            .iter()
            .filter_map(|value| Some((value.number?, value.name.as_deref()?)))
            .collect();
        for historical_value in &historical_enum.value {
            let value_name = historical_value
                .name
                .as_deref()
                .ok_or_else(|| CompatibilityError::missing_name("enum value", &qualified))?;
            let number = historical_value.number.ok_or_else(|| {
                CompatibilityError::missing_number("enum value", &qualify(&qualified, value_name))
            })?;
            let value_qualified = qualify(&qualified, value_name);
            let Some(current_value) = current_values_by_name.get(value_name) else {
                if let Some(reused) = current_values_by_number.get(&number) {
                    return Err(CompatibilityError::number_reused(
                        &value_qualified,
                        number,
                        reused,
                    ));
                }
                return Err(CompatibilityError::removed("enum value", &value_qualified));
            };
            if current_value.number != Some(number) {
                return Err(CompatibilityError::renumbered(
                    &value_qualified,
                    number,
                    current_value.number,
                ));
            }
        }
    }
    Ok(())
}

fn packed(field: &FieldDescriptorProto) -> Option<bool> {
    field.options.as_ref().and_then(|options| options.packed)
}

fn index_named<'a, T>(
    values: &'a [T],
    name: impl Fn(&'a T) -> Option<&'a str>,
) -> BTreeMap<&'a str, &'a T> {
    values
        .iter()
        .filter_map(|value| name(value).map(|name| (name, value)))
        .collect()
}

fn qualify(scope: &str, name: &str) -> String {
    if scope.is_empty() {
        name.to_owned()
    } else {
        format!("{scope}.{name}")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{detail}")]
pub(crate) struct CompatibilityError {
    detail: String,
}

impl CompatibilityError {
    fn removed(kind: &str, name: &str) -> Self {
        Self {
            detail: format!("removed historical {kind} {name}"),
        }
    }

    fn changed(kind: &str, name: &str) -> Self {
        Self {
            detail: format!("changed historical {kind} {name}"),
        }
    }

    fn missing_name(kind: &str, scope: &str) -> Self {
        Self {
            detail: format!("historical {kind} in {scope} has no name"),
        }
    }

    fn missing_number(kind: &str, name: &str) -> Self {
        Self {
            detail: format!("historical {kind} {name} has no number"),
        }
    }

    fn number_reused(name: &str, number: i32, reused: &str) -> Self {
        Self {
            detail: format!("historical number {number} for {name} was reused by {reused}"),
        }
    }

    fn renumbered(name: &str, expected: i32, observed: Option<i32>) -> Self {
        Self {
            detail: format!(
                "historical field or enum value {name} moved from {expected} to {observed:?}"
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use prost_types::{EnumValueDescriptorProto, FileDescriptorProto, field_descriptor_proto};

    #[test]
    fn additive_declarations_are_compatible() {
        let historical = fixture_descriptor();
        let mut current = historical.clone();
        current.file[0].message_type[0].field.push(field(
            "added",
            2,
            field_descriptor_proto::Type::String,
        ));
        current.file[0].enum_type[0]
            .value
            .push(enum_value("ADDED", 2));

        assert_eq!(require_compatible(&historical, &current), Ok(()));
    }

    #[test]
    fn rejects_field_renumbering_removal_and_type_changes() {
        let historical = fixture_descriptor();

        let mut renumbered = historical.clone();
        renumbered.file[0].message_type[0].field[0].number = Some(2);
        assert!(require_compatible(&historical, &renumbered).is_err());

        let mut removed = historical.clone();
        removed.file[0].message_type[0].field.clear();
        assert!(require_compatible(&historical, &removed).is_err());

        let mut changed_type = historical.clone();
        changed_type.file[0].message_type[0].field[0].r#type =
            Some(field_descriptor_proto::Type::String as i32);
        assert!(require_compatible(&historical, &changed_type).is_err());
    }

    #[test]
    fn rejects_enum_number_reuse_and_oneof_changes() {
        let historical = fixture_descriptor();

        let mut reused = historical.clone();
        reused.file[0].enum_type[0].value[1].name = Some("RENAMED".to_owned());
        assert!(require_compatible(&historical, &reused).is_err());

        let mut oneof_changed = historical.clone();
        oneof_changed.file[0].message_type[0].oneof_decl[0].name = Some("renamed".to_owned());
        assert!(require_compatible(&historical, &oneof_changed).is_err());
    }

    fn fixture_descriptor() -> FileDescriptorSet {
        FileDescriptorSet {
            file: vec![FileDescriptorProto {
                name: Some("fixture.proto".to_owned()),
                package: Some("fixture.v1".to_owned()),
                syntax: Some("proto3".to_owned()),
                message_type: vec![DescriptorProto {
                    name: Some("Message".to_owned()),
                    field: vec![FieldDescriptorProto {
                        oneof_index: Some(0),
                        ..field("value", 1, field_descriptor_proto::Type::Uint32)
                    }],
                    oneof_decl: vec![OneofDescriptorProto {
                        name: Some("choice".to_owned()),
                        ..Default::default()
                    }],
                    ..Default::default()
                }],
                enum_type: vec![EnumDescriptorProto {
                    name: Some("Kind".to_owned()),
                    value: vec![enum_value("UNSPECIFIED", 0), enum_value("VALUE", 1)],
                    ..Default::default()
                }],
                ..Default::default()
            }],
        }
    }

    fn field(name: &str, number: i32, kind: field_descriptor_proto::Type) -> FieldDescriptorProto {
        FieldDescriptorProto {
            name: Some(name.to_owned()),
            number: Some(number),
            label: Some(field_descriptor_proto::Label::Optional as i32),
            r#type: Some(kind as i32),
            ..Default::default()
        }
    }

    fn enum_value(name: &str, number: i32) -> EnumValueDescriptorProto {
        EnumValueDescriptorProto {
            name: Some(name.to_owned()),
            number: Some(number),
            ..Default::default()
        }
    }
}
