//! Shares standard-call argument checks within one immutable resolver pass.
//! A hash only selects possible repeats; exact bytes and the proven declaration
//! identity keep collisions, shadowing and distinct calling forms isolated.

use std::collections::BTreeMap;

use rootlight_ids::{ContentHash, SymbolId};
use rootlight_vba::ProcedureKind;

use crate::ResolutionError;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct Key {
    pub(super) target: SymbolId,
    pub(super) source_hash: ContentHash,
    pub(super) explicit: bool,
}

struct Entry<'a> {
    tail: &'a [u8],
    compatible: bool,
}

#[derive(Default)]
pub(super) struct Cache<'a> {
    entries: BTreeMap<Key, Vec<Entry<'a>>>,
}

impl<'a> Cache<'a> {
    /// Reuses only argument compatibility, never a site's target or source proof.
    /// The caller must first prove the immutable catalog declaration whose
    /// identity includes its complete parameter and procedure-kind contract.
    pub(super) fn matches(
        &mut self,
        key: Key,
        tail: &'a [u8],
        parameters: &[(&str, bool)],
        procedure: ProcedureKind,
        check: &mut impl FnMut() -> Result<(), ResolutionError>,
    ) -> Result<bool, ResolutionError> {
        check()?;
        let entries = self.entries.entry(key).or_default();
        for entry in entries.iter() {
            check()?;
            if entry.tail.len() != tail.len() {
                continue;
            }
            // Comparisons remain cancellable and charged even on cache hits.
            // Borrowing retained bytes avoids copying long argument expressions.
            let mut equal = true;
            for (left, right) in entry.tail.chunks(512).zip(tail.chunks(512)) {
                check()?;
                if left != right {
                    equal = false;
                    break;
                }
            }
            if equal {
                return Ok(entry.compatible);
            }
        }
        let compatible = super::super::calls::standard_arguments_match(
            tail,
            key.explicit,
            parameters,
            procedure,
            check,
        )?;
        entries.push(Entry { tail, compatible });
        Ok(compatible)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::ResolutionWorkBudget;
    use rootlight_cancel::{Cancellation, CancellationReason};
    use rootlight_ids::content_hash;

    fn key(target: u8, explicit: bool) -> Key {
        Key {
            target: SymbolId::from_bytes([target; 20]),
            source_hash: content_hash(b"one collision bucket"),
            explicit,
        }
    }

    #[test]
    fn cached_argument_checks_keep_contracts_forms_and_collisions_separate() {
        let mut cache = Cache::default();
        for _ in 0..2 {
            for (target, tail, explicit, parameters, kind, expected) in [
                (
                    1,
                    "(1)",
                    false,
                    &[("value", false)][..],
                    ProcedureKind::Function,
                    true,
                ),
                (
                    1,
                    "(,)",
                    false,
                    &[("value", false)][..],
                    ProcedureKind::Function,
                    false,
                ),
                (
                    1,
                    "()",
                    false,
                    &[("value", false)][..],
                    ProcedureKind::Function,
                    false,
                ),
                (
                    1,
                    " 1",
                    false,
                    &[("value", false)][..],
                    ProcedureKind::Function,
                    true,
                ),
                (
                    1,
                    " 1",
                    true,
                    &[("value", false)][..],
                    ProcedureKind::Function,
                    false,
                ),
                (2, "(1)", false, &[][..], ProcedureKind::Function, false),
                (
                    3,
                    "(1, 2)",
                    false,
                    &[("a", false), ("b", false)][..],
                    ProcedureKind::Function,
                    true,
                ),
                (
                    4,
                    "(ByVal 1)",
                    false,
                    &[("value", false)][..],
                    ProcedureKind::Function,
                    false,
                ),
                (
                    5,
                    "(ByVal 1)",
                    false,
                    &[("value", false)][..],
                    ProcedureKind::ExternalFunction,
                    true,
                ),
            ] {
                assert_eq!(
                    cache
                        .matches(
                            key(target, explicit),
                            tail.as_bytes(),
                            parameters,
                            kind,
                            &mut || Ok(())
                        )
                        .unwrap(),
                    expected
                );
            }
        }
    }

    #[test]
    fn repeated_argument_checks_save_scans_without_bypassing_work_or_cancellation() {
        let tail = format!("({})", "member".repeat(32));
        let parameters = [("value", false)];
        let mut cache = Cache::default();
        let mut first = ResolutionWorkBudget::new(4096);
        assert!(
            cache
                .matches(
                    key(1, false),
                    tail.as_bytes(),
                    &parameters,
                    ProcedureKind::Function,
                    &mut || first.consume()
                )
                .unwrap()
        );
        let mut repeated = ResolutionWorkBudget::new(4096);
        assert!(
            cache
                .matches(
                    key(1, false),
                    tail.as_bytes(),
                    &parameters,
                    ProcedureKind::Function,
                    &mut || repeated.consume()
                )
                .unwrap()
        );
        assert!(first.consumed() > 32);
        assert_eq!(repeated.consumed(), 3);
        let mut insufficient = ResolutionWorkBudget::new(repeated.consumed() - 1);
        assert!(matches!(
            cache.matches(
                key(1, false),
                tail.as_bytes(),
                &parameters,
                ProcedureKind::Function,
                &mut || insufficient.consume()
            ),
            Err(ResolutionError::WorkLimit { .. })
        ));
        let cancellation = Cancellation::new();
        cancellation.cancel(CancellationReason::ClientRequest);
        assert!(matches!(
            cache.matches(
                key(1, false),
                tail.as_bytes(),
                &parameters,
                ProcedureKind::Function,
                &mut || cancellation.check().map_err(ResolutionError::from)
            ),
            Err(ResolutionError::Cancelled(_))
        ));
    }

    #[test]
    fn long_collision_comparisons_check_every_chunk_and_propagate_cancellation() {
        let valid = format!("({})", "member".repeat(256));
        let invalid = format!("({}membe,)", "member".repeat(255));
        assert_eq!(valid.len(), invalid.len());
        let parameters = [("value", false)];
        let mut cache = Cache::default();
        assert!(
            cache
                .matches(
                    key(1, false),
                    valid.as_bytes(),
                    &parameters,
                    ProcedureKind::Function,
                    &mut || Ok(())
                )
                .unwrap()
        );
        let cancellation = Cancellation::new();
        let mut checks = 0;
        assert!(matches!(
            cache.matches(
                key(1, false),
                valid.as_bytes(),
                &parameters,
                ProcedureKind::Function,
                &mut || {
                    checks += 1;
                    if checks == 4 {
                        cancellation.cancel(CancellationReason::ClientRequest);
                    }
                    cancellation.check().map_err(ResolutionError::from)
                }
            ),
            Err(ResolutionError::Cancelled(_))
        ));
        for _ in 0..2 {
            assert!(
                !cache
                    .matches(
                        key(1, false),
                        invalid.as_bytes(),
                        &parameters,
                        ProcedureKind::Function,
                        &mut || Ok(())
                    )
                    .unwrap()
            );
        }
    }

    #[test]
    fn interrupted_argument_validation_does_not_cache_a_partial_result() {
        let parameters = [("value", false)];
        let mut cache = Cache::default();
        let mut budget = ResolutionWorkBudget::new(2);
        assert!(matches!(
            cache.matches(
                key(1, false),
                b"(member)",
                &parameters,
                ProcedureKind::Function,
                &mut || budget.consume()
            ),
            Err(ResolutionError::WorkLimit { .. })
        ));
        assert!(cache.entries.values().all(Vec::is_empty));
        assert!(
            cache
                .matches(
                    key(1, false),
                    b"(member)",
                    &parameters,
                    ProcedureKind::Function,
                    &mut || Ok(())
                )
                .unwrap()
        );
    }
}
