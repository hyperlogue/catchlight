//! Temp-then-rename writes, so an interrupted write never truncates the only
//! copy. The temp sits beside the target so the rename stays on one
//! filesystem, and its name is unique so concurrent writers of one target
//! never share it.

use std::ffi::OsString;
use std::io::{self, Write};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

static SEQUENCE: AtomicU64 = AtomicU64::new(0);

pub(crate) fn write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    write_with_mode(path, bytes, 0o666)
}

/// `mode` is the Unix permission bits the file is created with, before the
/// umask; it is ignored elsewhere.
pub(crate) fn write_with_mode(path: &Path, bytes: &[u8], mode: u32) -> io::Result<()> {
    let name = path.file_name().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} names no file", path.display()),
        )
    })?;
    let mut temp = OsString::from(".");
    temp.push(name);
    temp.push(format!(
        ".{}.{}.tmp",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    let temp = path.with_file_name(temp);

    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, mode);
    #[cfg(not(unix))]
    let _ = mode;
    let written = options.open(&temp).and_then(|mut file| {
        file.write_all(bytes)?;
        file.sync_all()
    });
    match written.and_then(|()| std::fs::rename(&temp, path)) {
        Ok(()) => Ok(()),
        Err(err) => {
            let _ = std::fs::remove_file(&temp);
            Err(err)
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Barrier};

    #[test]
    fn concurrent_writers_of_one_file_each_land_whole() {
        let dir = std::env::temp_dir().join(format!("catchlight-atomic-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("review.json");
        let writers = 8;
        let barrier = Arc::new(Barrier::new(writers));
        let threads: Vec<_> = (0..writers)
            .map(|i| {
                let (path, barrier) = (path.clone(), barrier.clone());
                std::thread::spawn(move || {
                    let bytes = vec![b'a' + i as u8; 1 << 20];
                    barrier.wait();
                    for _ in 0..8 {
                        super::write(&path, &bytes).unwrap();
                    }
                })
            })
            .collect();
        for thread in threads {
            thread.join().unwrap();
        }
        let kept = std::fs::read(&path).unwrap();
        assert_eq!(kept.len(), 1 << 20);
        assert!(kept.iter().all(|&b| b == kept[0]), "a write was torn");
        let names: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, ["review.json"], "no temp file is left behind");
        std::fs::remove_dir_all(&dir).ok();
    }
}
