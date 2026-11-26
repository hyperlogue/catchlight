//! Video encoding pipes straight-alpha RGBA frames to an external ffmpeg
//! (`CATCHLIGHT_FFMPEG`, else `ffmpeg` on PATH); no codec is linked.
use super::spec::{Background, VideoFormat, bad};
use crate::Error;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};

fn program() -> std::ffi::OsString {
    std::env::var_os("CATCHLIGHT_FFMPEG").unwrap_or_else(|| "ffmpeg".into())
}

pub fn preflight() -> Result<(), Error> {
    let status = Command::new(program())
        .arg("-version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    match status {
        Ok(s) if s.success() => Ok(()),
        _ => Err(bad(
            "video output needs ffmpeg on PATH, or CATCHLIGHT_FFMPEG naming it",
        )),
    }
}

pub struct VideoSink {
    child: Child,
    stdin: Option<ChildStdin>,
    stderr: Option<std::thread::JoinHandle<Vec<u8>>>,
    temp: Option<tempfile::TempPath>,
    path: PathBuf,
    frame_bytes: usize,
}
impl VideoSink {
    pub fn start(
        path: &Path,
        format: VideoFormat,
        size: [u32; 2],
        interval: f64,
        background: &Background,
    ) -> Result<Self, Error> {
        let temp =
            crate::file::temp_beside(path, &format!(".{}", format.extension()))?.into_temp_path();
        let mut command = Command::new(program());
        command
            .args(["-hide_banner", "-loglevel", "error", "-nostdin", "-y"])
            .args(["-f", "rawvideo", "-pix_fmt", "rgba"])
            .args(["-s", &format!("{}x{}", size[0], size[1])])
            .args(["-framerate", &format!("{:.9}", 1.0 / interval)])
            .args(["-i", "pipe:0", "-an"]);
        match format {
            VideoFormat::Mp4 => {
                let [r, g, b] = background
                    .srgb
                    .map(|c| (c * 255.0).round().clamp(0.0, 255.0) as u8);
                command
                    .args([
                        "-vf",
                        &format!("pad=ceil(iw/2)*2:ceil(ih/2)*2:color=0x{r:02x}{g:02x}{b:02x}"),
                    ])
                    .args(["-c:v", "libx264", "-pix_fmt", "yuv420p", "-crf", "18"])
                    .args(["-movflags", "+faststart", "-f", "mp4"]);
            }
            VideoFormat::Webm => {
                command
                    .args(["-c:v", "libvpx-vp9", "-pix_fmt", "yuva420p"])
                    .args(["-b:v", "0", "-crf", "30", "-row-mt", "1", "-f", "webm"]);
            }
            VideoFormat::Mkv => {
                command
                    .args(["-c:v", "ffv1", "-level", "3", "-slices", "16"])
                    .args(["-pix_fmt", "bgra", "-f", "matroska"]);
            }
            VideoFormat::Gif => {
                command
                    .args([
                        "-filter_complex",
                        "split[a][b];[a]palettegen=reserve_transparent=1[p];[b][p]paletteuse",
                    ])
                    .args(["-loop", "0", "-f", "gif"]);
            }
        }
        command
            .arg(temp.as_os_str())
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        let mut child = command.spawn().map_err(|source| Error::Spawn {
            program: program(),
            source,
        })?;
        let stdin = child.stdin.take();
        // Drain stderr concurrently so a chatty encoder cannot block the pipe.
        let stderr = child.stderr.take().map(|mut pipe| {
            std::thread::spawn(move || {
                let mut out = Vec::new();
                let _ = pipe.read_to_end(&mut out);
                out
            })
        });
        Ok(Self {
            child,
            stdin,
            stderr,
            temp: Some(temp),
            path: path.to_path_buf(),
            frame_bytes: size[0] as usize * size[1] as usize * 4,
        })
    }

    pub fn push(&mut self, pixels: &[u8]) -> Result<(), Error> {
        if pixels.len() != self.frame_bytes {
            return Err(bad("video frame size changed"));
        }
        let stdin = self.stdin.as_mut().ok_or_else(|| bad("ffmpeg closed"))?;
        if stdin.write_all(pixels).is_err() {
            self.stdin = None;
            return Err(self.failure());
        }
        Ok(())
    }

    pub fn finish(mut self) -> Result<(), Error> {
        self.stdin = None;
        let status = self.child.wait().map_err(|source| Error::Wait {
            program: program(),
            source,
        })?;
        if !status.success() {
            return Err(self.failure());
        }
        let path = self.path.clone();
        let temp = self
            .temp
            .take()
            .ok_or_else(|| bad("video already finished"))?;
        crate::file::persist(temp, &path)
    }

    fn failure(&mut self) -> Error {
        let status = match self.child.wait() {
            Ok(status) => status.to_string(),
            Err(e) => format!("status unknown: {e}"),
        };
        let message = self
            .stderr
            .take()
            .and_then(|t| t.join().ok())
            .map(|b| String::from_utf8_lossy(&b).trim().to_owned())
            .unwrap_or_default();
        bad(format!(
            "ffmpeg failed encoding {} ({status}): {message}",
            self.path.display()
        ))
    }
}
impl Drop for VideoSink {
    fn drop(&mut self) {
        // The temp file is removed by its own drop.
        self.stdin = None;
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
