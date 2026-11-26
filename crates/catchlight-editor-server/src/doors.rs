//! What the HTTP and Unix-socket doors share: the accept loop's filter, the
//! connection cap, the line reader, and the answer to a line that is not a
//! request.

use std::io::{self, BufRead, BufReader, Read};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use catchlight_editor_protocol::{ErrorCode, Reply, RequestId};

/// One socket line, or one HTTP body that is not an upload.
pub(crate) const MAX_REQUEST_BYTES: usize = 1024 * 1024;
/// Connections one door serves at once.
pub(crate) const MAX_CONNECTIONS: usize = 64;
const ACCEPT_RETRY_PAUSE: Duration = Duration::from_millis(50);

/// The connections `incoming` yields, past any that failed to arrive: a
/// refused accept (an aborted handshake, no descriptors left) is no reason to
/// stop serving the tabs already open. The pause keeps a persistent failure
/// from spinning.
pub(crate) fn accepted<S>(
    incoming: impl Iterator<Item = io::Result<S>>,
) -> impl Iterator<Item = S> {
    incoming.filter_map(|stream| match stream {
        Ok(stream) => Some(stream),
        Err(err) => {
            tracing::warn!("accepting a connection failed: {err}");
            std::thread::sleep(ACCEPT_RETRY_PAUSE);
            None
        }
    })
}

#[derive(Default)]
pub(crate) struct ConnectionLimiter {
    active: AtomicUsize,
}

impl ConnectionLimiter {
    pub(crate) fn try_acquire(self: &Arc<Self>) -> Option<ConnectionPermit> {
        self.active
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |active| {
                (active < MAX_CONNECTIONS).then_some(active + 1)
            })
            .ok()
            .map(|_| ConnectionPermit(self.clone()))
    }
}

pub(crate) struct ConnectionPermit(Arc<ConnectionLimiter>);

impl Drop for ConnectionPermit {
    fn drop(&mut self) {
        self.0.active.fetch_sub(1, Ordering::AcqRel);
    }
}

/// What [`read_line`] found.
pub(crate) enum Line {
    /// A line through its `\n`, which it keeps.
    Terminated(Vec<u8>),
    /// The input ended partway through a line.
    Unterminated(Vec<u8>),
    /// The input ended between lines.
    End,
}

pub(crate) enum LineError {
    /// The line would take more bytes than the budget has left.
    TooLong,
    Io,
}

/// Takes every byte of the line out of `budget`. `wait` runs before each
/// read that has to go to the stream, with nothing buffered.
pub(crate) fn read_line<S: Read>(
    reader: &mut BufReader<S>,
    budget: &mut usize,
    mut wait: impl FnMut(&S) -> io::Result<()>,
) -> Result<Line, LineError> {
    let mut line = Vec::new();
    loop {
        if reader.buffer().is_empty() {
            wait(reader.get_ref()).map_err(|_| LineError::Io)?;
        }
        let available = reader.fill_buf().map_err(|_| LineError::Io)?;
        if available.is_empty() {
            return Ok(if line.is_empty() {
                Line::End
            } else {
                Line::Unterminated(line)
            });
        }
        let take = available
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(available.len(), |pos| pos + 1);
        if take > *budget {
            return Err(LineError::TooLong);
        }
        *budget -= take;
        let terminated = available[take - 1] == b'\n';
        line.extend_from_slice(&available[..take]);
        reader.consume(take);
        if terminated {
            return Ok(Line::Terminated(line));
        }
    }
}

/// A line that is not a request is still answered against the id the
/// client is blocked on, or it would wait forever.
pub(crate) fn refuse_line(line: &[u8], message: String) -> Reply {
    Reply::Err {
        op_index: None,
        limit: None,
        id: serde_json::from_slice::<RequestId>(line).map_or(0, |r| r.id),
        code: ErrorCode::BadRequest,
        message,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failed_accept_does_not_end_the_connections_after_it() {
        let incoming = [
            Err(io::Error::from(io::ErrorKind::ConnectionAborted)),
            Ok(1),
            Err(io::Error::other("no descriptors left")),
            Ok(2),
        ];
        assert_eq!(accepted(incoming.into_iter()).collect::<Vec<_>>(), [1, 2]);
    }

    #[test]
    fn a_connection_limit_releases_capacity_on_drop() {
        let limiter = Arc::new(ConnectionLimiter::default());
        let mut permits = (0..MAX_CONNECTIONS)
            .map(|_| limiter.try_acquire().unwrap())
            .collect::<Vec<_>>();
        assert!(limiter.try_acquire().is_none());

        permits.pop();

        assert!(limiter.try_acquire().is_some());
    }
}
