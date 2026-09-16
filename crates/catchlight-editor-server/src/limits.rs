//! The serialized request budget also applies to in-process and wasm callers.
//! Counting stops at the first excess; no equally large temporary JSON buffer
//! is allocated just to measure it. Attachments have separate load budgets.

use crate::EditorError;
use serde::Serialize;

pub const MAX_REQUEST_JSON_BYTES: usize = 1024 * 1024;

pub(super) fn json_size(
    value: &impl Serialize,
    resource: &'static str,
    limit: usize,
) -> Result<(), EditorError> {
    struct Count {
        bytes: usize,
        limit: usize,
    }
    impl std::io::Write for Count {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.bytes = self.bytes.saturating_add(bytes.len());
            if self.bytes > self.limit {
                return Err(std::io::Error::other("JSON size limit"));
            }
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut count = Count { bytes: 0, limit };
    if let Err(error) = serde_json::to_writer(&mut count, value) {
        if count.bytes > limit {
            return Err(EditorError::Limit {
                resource,
                limit: limit as u64,
                requested: count.bytes as u64,
            });
        }
        return Err(EditorError::BadRequest(error.to_string()));
    }
    Ok(())
}
