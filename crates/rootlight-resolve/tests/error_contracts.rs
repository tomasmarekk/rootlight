//! Public error contracts shared by resolver and foreign-link consumers.
//!
//! These checks pin diagnostics, typed causes and transparent cancellation
//! independently of the implementation used to provide the standard traits.

use std::{error::Error, fmt};

use rootlight_cancel::{Cancellation, CancellationReason};
use rootlight_ir::{FactIdentityRecipeError, IrDocumentValidationError, IrValidationError};
use rootlight_resolve::{
    DynamicCallCalibrationError, ForeignLinkError, ForeignLinkInputError, ForeignLinkLimitError,
    QualityError, ResolutionError, ResolutionLimitError,
};

struct FailingWriter;

impl fmt::Write for FailingWriter {
    fn write_str(&mut self, _: &str) -> fmt::Result {
        Err(fmt::Error)
    }
}

fn assert_display(error: &dyn Error, expected: &str) {
    assert_eq!(error.to_string(), expected);
    assert_eq!(format!("{error:#}"), expected);
    assert_eq!(
        fmt::write(&mut FailingWriter, format_args!("{error}")),
        Err(fmt::Error)
    );
}

fn assert_leaf(error: impl Error + Send + Sync + 'static, expected: &str) {
    assert_display(&error, expected);
    assert!(error.source().is_none());
}

fn assert_cause<E: Error + PartialEq + 'static>(error: &dyn Error, expected: E) {
    let cause = error.source().expect("wrapper retains its typed cause");
    assert_eq!(cause.downcast_ref::<E>(), Some(&expected));
    assert!(cause.source().is_none());
}

#[test]
fn configuration_errors_retain_messages_without_causes() {
    assert_leaf(
        ResolutionLimitError,
        "resolver limits exceed their checked ceilings",
    );
    assert_leaf(
        ForeignLinkLimitError,
        "foreign-link limits exceed the supported bounded range",
    );
    for (error, message) in [
        (
            DynamicCallCalibrationError::UnsupportedLanguage,
            "dynamic call calibration language is unsupported",
        ),
        (
            DynamicCallCalibrationError::PrecisionBelowFloor,
            "dynamic call precision must be between 9000 and 10000 basis points",
        ),
        (
            DynamicCallCalibrationError::EmptyCorpus,
            "dynamic call calibration corpus must not be empty",
        ),
        (
            DynamicCallCalibrationError::HiddenExactBinding,
            "dynamic call calibration must not contain hidden exact bindings",
        ),
    ] {
        assert_leaf(error, message);
    }
    for (error, message) in [
        (
            ForeignLinkInputError::PredicateMismatch,
            "foreign-link predicate is incompatible with its evidence namespace",
        ),
        (
            ForeignLinkInputError::ZeroConfidence,
            "foreign-link confidence must be greater than zero",
        ),
    ] {
        assert_leaf(error, message);
    }
    for (error, message) in [
        (
            QualityError::DuplicateExpectation,
            "resolution quality corpus contains a duplicate expectation",
        ),
        (
            QualityError::DuplicateDecision,
            "resolution batch contains a duplicate decision",
        ),
        (
            QualityError::CountOverflow,
            "resolution quality counter overflowed",
        ),
    ] {
        assert_leaf(error, message);
    }
}

#[test]
fn resolution_failures_retain_messages_without_causes() {
    for (error, message) in [
        (
            ResolutionError::ClassicSourceIdentity,
            "classic project source identity differs from the indexed generation",
        ),
        (
            ResolutionError::CountOverflow,
            "resolver result count is not representable",
        ),
        (
            ResolutionError::InvalidScore,
            "resolver score escaped the confidence range",
        ),
        (
            ResolutionError::UnsupportedIdentityRemap,
            "resolver application requires an unsupported dependent identity remap",
        ),
    ] {
        assert_leaf(error, message);
    }
    for maximum in [0, 1, usize::MAX] {
        assert_leaf(
            ResolutionError::WorkLimit { maximum },
            &format!("resolver work exceeds {maximum} units"),
        );
    }
}

#[test]
fn foreign_link_failures_retain_messages_without_causes() {
    for (error, message) in [
        (
            ForeignLinkError::InvalidInput,
            "foreign-link input violates its typed evidence contract",
        ),
        (
            ForeignLinkError::UnknownEvidence,
            "foreign-link evidence references an unknown fact",
        ),
        (
            ForeignLinkError::SourceMismatch,
            "foreign-link source does not match the normalized generation",
        ),
        (
            ForeignLinkError::EmptyGroup,
            "foreign-link candidate group is empty",
        ),
        (
            ForeignLinkError::ResourceLimit,
            "foreign-link evidence exceeds its resource limit",
        ),
        (
            ForeignLinkError::CountOverflow,
            "foreign-link candidate count is not representable",
        ),
    ] {
        assert_leaf(error, message);
    }
}

#[test]
fn resolution_failures_retain_downcastable_causes() {
    let error = ResolutionError::InvalidDocument(IrDocumentValidationError::NonCanonicalDocument);
    assert_display(&error, "resolver input document is invalid");
    assert_cause(&error, IrDocumentValidationError::NonCanonicalDocument);
    let error = ResolutionError::InvalidProducer(IrValidationError::InvalidSourceSpan);
    assert_display(&error, "resolver producer identity is invalid");
    assert_cause(&error, IrValidationError::InvalidSourceSpan);
    let error = ResolutionError::FactIdentity(FactIdentityRecipeError);
    assert_display(&error, "resolver fact identity could not be derived");
    assert_cause(&error, FactIdentityRecipeError);
}

#[test]
fn foreign_link_failures_retain_downcastable_causes() {
    let error = ForeignLinkError::InvalidDocument(IrDocumentValidationError::NonCanonicalDocument);
    assert_display(&error, "foreign-link document is invalid");
    assert_cause(&error, IrDocumentValidationError::NonCanonicalDocument);
    let error = ForeignLinkError::InvalidProducer(IrValidationError::InvalidSourceSpan);
    assert_display(&error, "foreign-link producer identity is invalid");
    assert_cause(&error, IrValidationError::InvalidSourceSpan);
    let error = ForeignLinkError::FactIdentity(FactIdentityRecipeError);
    assert_display(&error, "foreign-link fact identity could not be derived");
    assert_cause(&error, FactIdentityRecipeError);
}

#[test]
fn cancellation_conversion_preserves_reason_and_transparent_error_chain() {
    fn resolve(token: &Cancellation) -> Result<(), ResolutionError> {
        token.check()?;
        Ok(())
    }
    fn link(token: &Cancellation) -> Result<(), ForeignLinkError> {
        token.check()?;
        Ok(())
    }
    for reason in [
        CancellationReason::ClientRequest,
        CancellationReason::ParentCancelled,
        CancellationReason::DeadlineExceeded,
        CancellationReason::Shutdown,
        CancellationReason::ResourceLimit,
    ] {
        let token = Cancellation::new();
        assert!(resolve(&token).is_ok());
        assert!(link(&token).is_ok());
        assert!(token.cancel(reason));
        let resolution = resolve(&token).unwrap_err();
        let foreign = link(&token).unwrap_err();
        assert!(
            matches!(resolution, ResolutionError::Cancelled(inner) if inner.reason() == reason)
        );
        assert!(matches!(foreign, ForeignLinkError::Cancelled(inner) if inner.reason() == reason));
        let message = token.check().unwrap_err().to_string();
        assert_leaf(resolution, &message);
        assert_leaf(foreign, &message);
    }
}
