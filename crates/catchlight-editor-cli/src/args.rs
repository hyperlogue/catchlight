//! Value parsers for flags and the input files commands read.

use std::io::Read;

use anyhow::{Context, Result, anyhow, bail};
use catchlight_editor_protocol::{NodeKindArg, ParamId, ParamPose, SlotId, SlotPair};

pub const MAX_COMMAND_BYTES: usize = 1024 * 1024;

/// Parses as the wire does, so serde's error names the alternatives.
pub fn wire_enum<T: serde::de::DeserializeOwned>(s: &str) -> std::result::Result<T, String> {
    serde_json::from_value(serde_json::Value::String(s.to_owned())).map_err(|e| e.to_string())
}

pub fn parse_kind(s: &str) -> Result<NodeKindArg> {
    Ok(match s.to_ascii_lowercase().as_str() {
        "group" => NodeKindArg::Group,
        "part" => NodeKindArg::Part,
        "composite" => NodeKindArg::Composite,
        "motion_response" => NodeKindArg::MotionResponse,
        "meshgroup" | "mesh_group" => NodeKindArg::MeshGroup,
        other => {
            bail!("unknown node kind {other:?} (group|part|composite|meshgroup|motion_response)")
        }
    })
}

pub fn parse_floats<const N: usize>(s: &str) -> Result<[f32; N]> {
    let parts: Vec<f32> = s
        .split(',')
        .map(|p| p.trim().parse::<f32>())
        .collect::<std::result::Result<_, _>>()
        .map_err(|e| anyhow!("bad number list {s:?}: {e}"))?;
    parts
        .try_into()
        .map_err(|_| anyhow!("expected {N} comma-separated numbers, got {s:?}"))
}

pub fn parse_vec3(s: &str) -> Result<[f32; 3]> {
    parse_floats::<3>(s)
}

pub fn parse_vec2(s: &str) -> Result<[f32; 2]> {
    parse_floats::<2>(s)
}

pub fn read_json(path: &str) -> Result<String> {
    if path == "-" {
        read_command_json(std::io::stdin().lock())
    } else {
        let file = std::fs::File::open(path).with_context(|| format!("reading {path}"))?;
        read_command_json(file).with_context(|| format!("reading {path}"))
    }
}

fn read_command_json(reader: impl Read) -> Result<String> {
    let mut bytes = Vec::new();
    reader
        .take(MAX_COMMAND_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_COMMAND_BYTES {
        bail!(
            "JSON input exceeds the 1 MiB command limit; split independent edits into explicit batches"
        );
    }
    Ok(String::from_utf8(bytes)?)
}

pub fn parse_cell(s: &str) -> Result<[u32; 2]> {
    let bad = || anyhow!("cell must look like x,y, got {s:?}");
    let (x, y) = s.split_once(',').ok_or_else(bad)?;
    Ok([
        x.trim().parse().map_err(|_| bad())?,
        y.trim().parse().map_err(|_| bad())?,
    ])
}

pub fn parse_size(s: &str) -> Result<[u32; 2]> {
    let bad = || anyhow!("size must look like 512x512, got {s:?}");
    let (w, h) = s.split_once(['x', 'X']).ok_or_else(bad)?;
    Ok([
        w.trim().parse().map_err(|_| bad())?,
        h.trim().parse().map_err(|_| bad())?,
    ])
}

/// The server's working directory is not this process's.
pub fn absolute(path: &str) -> Result<String> {
    Ok(std::path::absolute(path)?.display().to_string())
}

pub fn parse_param(s: &str) -> Result<ParamPose> {
    let (id, value) = s
        .split_once('=')
        .ok_or_else(|| anyhow!("pose must look like <param id>=<value>, got {s:?}"))?;
    Ok(ParamPose {
        param: ParamId::new(id.trim())?,
        value: value
            .trim()
            .parse::<f32>()
            .map_err(|e| anyhow!("bad param value {value:?}: {e}"))?,
    })
}

pub fn parse_pair(s: &str) -> Result<SlotPair> {
    let bad = || anyhow!("a pair must look like <a slot>:<b slot>:<weight>, got {s:?}");
    let [a, b, weight] = s.split(':').collect::<Vec<_>>()[..] else {
        return Err(bad());
    };
    Ok(SlotPair {
        a: SlotId::new(a.trim())?,
        b: SlotId::new(b.trim())?,
        weight: weight
            .trim()
            .parse::<f32>()
            .map_err(|e| anyhow!("bad weight {weight:?}: {e}"))?,
    })
}

pub fn read_offsets(path: &str) -> Result<Vec<[f32; 2]>> {
    let text = std::fs::read_to_string(path).with_context(|| format!("reading {path}"))?;
    serde_json::from_str(&text).with_context(|| format!("parsing {path}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::build_command;
    use crate::cli::Cli;
    use catchlight_editor_protocol::Command;
    use clap::Parser;

    #[test]
    fn command_json_stops_reading_at_the_input_limit() {
        let mut source = std::io::repeat(b' ').take(u64::MAX);
        let error = read_command_json(&mut source).unwrap_err();
        assert!(error.to_string().contains("1 MiB command limit"));
        assert_eq!(source.limit(), u64::MAX - MAX_COMMAND_BYTES as u64 - 1);
        let exact = std::io::repeat(b' ').take(MAX_COMMAND_BYTES as u64);
        assert_eq!(read_command_json(exact).unwrap().len(), MAX_COMMAND_BYTES);
        assert!(read_command_json(&[0xff][..]).is_err());
    }

    #[test]
    fn a_missing_input_file_is_named_in_the_error() {
        let missing = "no-such-dir/edits.json";
        for error in [
            read_json(missing).unwrap_err(),
            read_offsets(missing).unwrap_err(),
        ] {
            assert!(format!("{error:#}").contains(missing), "{error:#}");
        }
    }

    #[test]
    fn a_malformed_value_is_refused_while_parsing_the_arguments() {
        for (args, want) in [
            (
                &["preview", "--out", "p.png", "--param", "head.x"][..],
                "head.x",
            ),
            (&["preview", "--out", "p.png", "--size", "512"], "512x512"),
            (
                &["preview", "--out", "p.png", "--camera-center", "0;1"],
                "0;1",
            ),
            (&["node", "set", "hat", "--translate", "1,2"], "3"),
            (
                &["node", "add", "--parent", "root", "--kind", "blob"],
                "blob",
            ),
            (
                &["weld", "set", "--a", "a", "--b", "b", "--pair", "s1:s2"],
                "s1:s2",
            ),
            (
                &[
                    "binding", "unset", "--param", "p", "--node", "n", "--target", "tx", "--cell",
                    "1,x",
                ],
                "1,x",
            ),
        ] {
            let mut argv = vec!["cli", "--session", "1"];
            argv.extend(args);
            let Err(err) = Cli::try_parse_from(&argv) else {
                panic!("{args:?} parses");
            };
            assert!(err.to_string().contains(want), "{args:?}: {err}");
        }
        let cli = Cli::try_parse_from([
            "cli",
            "--session",
            "1",
            "preview",
            "--out",
            "p.png",
            "--param",
            "head.x=-0.5",
            "--size",
            "64x32",
            "--camera-height",
            "100",
            "--camera-center",
            "0, 120",
        ])
        .unwrap();
        match build_command(&cli).unwrap() {
            Command::Preview {
                pose, size, camera, ..
            } => {
                assert_eq!(pose[0].value, -0.5);
                assert_eq!(size, Some([64, 32]));
                assert_eq!(camera.unwrap().center, [0.0, 120.0]);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_pose_names_a_param_by_id() {
        let pose = parse_param("head.x=0.25").unwrap();
        assert_eq!(pose.param.as_str(), "head.x");
        assert!((pose.value - 0.25).abs() < 1e-6);
        assert!(parse_param("head.x").is_err(), "a pose needs a value");
        assert!(parse_param("head x=1").is_err(), "a pose needs a valid id");
    }
}
