//! Shared source-occurrence discriminators for structural and project lowering.
//! Both producers must use the same domain separation and parent framing so
//! retained declarations keep one identity when project analysis is added.

/// Derives a grammar-reviewed source-occurrence discriminator.
///
/// The stable context separates language/construct domains. The caller supplies
/// the enclosing scope discriminator, canonical syntax kind and zero-based
/// occurrence within the reviewed sibling group. Byte offsets and source bodies
/// are deliberately absent; this identifies written occurrences, not runtime values.
#[must_use]
pub fn derive_structural_occurrence_identity(
    context: &str,
    parent: Option<[u8; 32]>,
    kind: &str,
    position: u64,
) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new_derive_key(context);
    match parent {
        Some(parent) => {
            hasher.update(&[1]);
            hasher.update(&parent);
        }
        None => {
            hasher.update(&[0]);
        }
    }
    hasher.update(&position.to_be_bytes());
    hasher.update(kind.as_bytes());
    *hasher.finalize().as_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn occurrence_identity_frames_each_semantic_input() {
        let expected = derive_structural_occurrence_identity("scope", None, "kind", 0);
        assert_eq!(
            expected,
            derive_structural_occurrence_identity("scope", None, "kind", 0)
        );
        for other in [
            derive_structural_occurrence_identity("other", None, "kind", 0),
            derive_structural_occurrence_identity("scope", Some([0; 32]), "kind", 0),
            derive_structural_occurrence_identity("scope", None, "other", 0),
            derive_structural_occurrence_identity("scope", None, "kind", 1),
        ] {
            assert_ne!(expected, other);
        }
    }
}
