use std::error::Error as _;

use crate::RendererError;

#[test]
fn a_wrapped_error_is_printed_once_in_a_chain() {
    let error = RendererError::from(catchlight_core::LoadLimitError {
        resource: "decoded_texture_bytes",
        limit: 1,
        got: 2,
    });
    let mut chain = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        chain += &format!(": {cause}");
        source = cause.source();
    }
    assert_eq!(
        chain,
        "preparing a model's textures: decoded_texture_bytes exceeded load limit: 2 > 1",
    );
}
