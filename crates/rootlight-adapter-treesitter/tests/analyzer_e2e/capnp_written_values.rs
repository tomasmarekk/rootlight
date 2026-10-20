//! Written constant and annotation uses retain their exact occurrence namespace.
//! Type-looking prefixes in value expressions never prove a schema type binding.

use super::*;

#[test]
fn capnp_value_and_annotation_names_never_become_type_use_prefixes() {
    let source = "annotation tag(struct) :Text; struct Item $tag(\"label\") {} const first :Item = (); const second :Item = Item.member;\n";
    let result = output(source);
    let item = entity(&result, "Item", EntityKind::Struct);
    linked_type(&result, source, "first :Item", "Item", item.id);
    linked_type(&result, source, "second :Item", "Item", item.id);
    let uses = result
        .document()
        .occurrences
        .iter()
        .filter(|row| row.role == OccurrenceRole::TypeUse)
        .collect::<Vec<_>>();
    assert_eq!(uses.len(), 2);
    let value_start = source.find("= Item.member").unwrap() + 2;
    assert!(
        uses.iter()
            .all(|row| row.source.span().start_byte() < u64::try_from(value_start).unwrap())
    );
    let annotation_start = source.find("$tag").unwrap() + 1;
    assert!(
        result
            .document()
            .occurrences
            .iter()
            .any(|row| row.role == OccurrenceRole::Reference
                && row.source.span().start_byte() == u64::try_from(annotation_start).unwrap()
                && text(source, &row.source) == "tag"
                && matches!(row.target, OccurrenceTarget::Unresolved { .. }))
    );
    let values = result
        .document()
        .occurrences
        .iter()
        .filter(|row| {
            row.role == OccurrenceRole::Reference
                && row.source.span().start_byte() >= u64::try_from(value_start).unwrap()
        })
        .collect::<Vec<_>>();
    assert!(!values.is_empty());
    assert!(values.iter().all(|row| matches!(
        text(source, &row.source),
        "Item" | "member" | "Item.member"
    ) && matches!(
        row.target,
        OccurrenceTarget::Unresolved { .. }
    )));
}
