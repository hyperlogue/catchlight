//! Kitty support is confirmed by its graphics query, never inferred from TERM.
//! Redirected stdout receives neither queries nor graphics escapes. The wire
//! format follows https://sw.kovidgoyal.net/kitty/graphics-protocol/.
use base64::Engine;
use std::io::{IsTerminal, Write};

#[cfg(unix)]
const QUERY: &[u8] = b"\x1b_Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\";
#[cfg(any(unix, test))]
const OK: &[u8] = b"\x1b_Gi=31;OK\x1b\\";

pub fn supported() -> bool {
    if !std::io::stdout().is_terminal() {
        return false;
    }
    #[cfg(unix)]
    {
        query().unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        false
    }
}
#[cfg(unix)]
fn query() -> std::io::Result<bool> {
    use rustix::event::{poll, PollFd, PollFlags, Timespec};
    use rustix::termios::{tcgetattr, tcsetattr, LocalModes, OptionalActions, SpecialCodeIndex};
    use std::io::Read;
    let mut tty = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/tty")?;
    let original = tcgetattr(&tty)?;
    let mut mode = original.clone();
    mode.local_modes
        .remove(LocalModes::ICANON | LocalModes::ECHO);
    mode.special_codes[SpecialCodeIndex::VMIN] = 0;
    mode.special_codes[SpecialCodeIndex::VTIME] = 0;
    tcsetattr(&tty, OptionalActions::Now, &mode)?;
    let result = (|| {
        std::io::stdout().write_all(QUERY)?;
        std::io::stdout().flush()?;
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(200);
        let mut response = Vec::new();
        while std::time::Instant::now() < deadline && response.len() < 4096 {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            let timeout = Timespec {
                tv_sec: 0,
                tv_nsec: left.as_nanos().min(200_000_000) as _,
            };
            let mut fd = [PollFd::new(&tty, PollFlags::IN)];
            if poll(&mut fd, Some(&timeout))? == 0 {
                break;
            }
            let mut bytes = [0_u8; 256];
            let n = tty.read(&mut bytes)?;
            if n == 0 {
                break;
            }
            response.extend_from_slice(&bytes[..n]);
            if acknowledged(&response) {
                return Ok(true);
            }
        }
        Ok(false)
    })();
    // Restore on success, timeout and I/O failure before returning to the shell.
    let restore = tcsetattr(&tty, OptionalActions::Now, &original).map_err(std::io::Error::from);
    result.and_then(|value| restore.map(|_| value))
}
#[cfg(any(unix, test))]
fn acknowledged(bytes: &[u8]) -> bool {
    bytes.windows(OK.len()).any(|window| window == OK)
}

pub fn present(png: &[u8], mut out: impl Write) -> std::io::Result<()> {
    let encoded = base64::engine::general_purpose::STANDARD.encode(png);
    let chunks = encoded.as_bytes().chunks(4096);
    let count = chunks.len();
    for (i, chunk) in chunks.enumerate() {
        if i == 0 {
            write!(out, "\x1b_Ga=T,f=100,q=2,m={};", u8::from(i + 1 < count))?;
        } else {
            write!(out, "\x1b_Gm={};", u8::from(i + 1 < count))?;
        }
        out.write_all(chunk)?;
        out.write_all(b"\x1b\\")?;
    }
    out.write_all(b"\n")?;
    out.flush()
}
#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;
    #[test]
    fn query_needs_an_explicit_matching_success_response() {
        assert!(acknowledged(b"\x1b_Gi=31;OK\x1b\\"));
        for bytes in [
            &b"\x1b_Gi=32;OK\x1b\\"[..],
            b"\x1b_Gi=31;ERROR\x1b\\",
            b"\x1b[?1;2c",
            b"OK",
            b"\x1b_Gi=31;OK",
        ] {
            assert!(!acknowledged(bytes));
        }
    }
    #[test]
    fn image_chunks_round_trip_and_have_one_final_chunk() {
        let input = vec![123_u8; 12_000];
        let mut output = Vec::new();
        assert!(present(&input, &mut output).is_ok());
        let text = String::from_utf8_lossy(&output);
        let mut payload = String::new();
        let mut finals = 0;
        for chunk in text.trim_end().split("\x1b\\").filter(|c| !c.is_empty()) {
            let (header, data) = chunk
                .split_once(';')
                .expect("chunk has a payload separator");
            assert!(data.len() <= 4096);
            assert_eq!(data.len() % 4, 0);
            payload.push_str(data);
            if header.ends_with("m=0") {
                finals += 1;
            }
        }
        assert_eq!(finals, 1);
        assert_eq!(
            base64::engine::general_purpose::STANDARD
                .decode(payload)
                .ok(),
            Some(input)
        );
    }
}
