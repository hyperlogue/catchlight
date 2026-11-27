use std::error::Error as _;

use crate::RendererError;

#[test]
fn a_wrapped_error_is_printed_once_in_a_chain() {
    let limit = catchlight_core::LoadLimitError {
        resource: catchlight_core::LoadResource::DecodedTextureBytes,
        limit: 1,
        got: 2,
    };
    let cause = limit.to_string();
    let error = RendererError::from(limit);
    let mut chain = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        chain += &format!(": {cause}");
        source = cause.source();
    }
    assert_eq!(chain.matches(&cause).count(), 1, "{chain}");
}
