//! Adapts the shared first-party VBA name reader to adapter cancellation errors.
//! The resolver uses the same spelling rules without depending on the native parser.

use rootlight_adapter_sdk::AdapterError;
use rootlight_cancel::Cancellation;
use rootlight_vba::ProcedurePath;

pub(super) fn parse<'a>(
    source: &'a [u8],
    cancellation: &Cancellation,
) -> Result<Option<ProcedurePath<'a>>, AdapterError> {
    rootlight_vba::procedure_pointer_path(source, &mut || {
        cancellation.check().map_err(AdapterError::from)
    })
}
