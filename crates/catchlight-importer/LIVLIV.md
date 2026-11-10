# LivLiv import status

The `livliv` module validates LivLiv v4 packages and converts a limited,
explicitly lossy subset. **CL-14 is not complete:** procedural motion, output
watermarks and visual comparison against the official player remain open.
The runtime still loads `.clm` only. All conversion logic lives in
`catchlight-importer`; `xtask` supplies filesystem IO and command arguments.

The contract is pinned to LivLiv Player revision
[`d999dea88981124168f9e9ab948eda8d2f23c453`](https://github.com/livliv-dev/player/tree/d999dea88981124168f9e9ab948eda8d2f23c453).
The two bundled JSON schemas are Apache-2.0; see
[third-party notices](../../THIRD-PARTY-NOTICES.md).

## Usage

Inspect a directory or ZIP package without creating a model:

```sh
cargo xtask import-livliv /path/to/character --inspect --report report.json
cargo xtask import-livliv /path/to/character.livliv --inspect
```

Convert a package that does not require a watermark, after reviewing losses:

```sh
cargo xtask import-livliv /path/to/character.livliv --allow-lossy \
  -o character.clm --report report.json
```

The command prints its report before attempting conversion. `--inspect`
succeeds when package validation passes even if the report has conversion
blockers. Conversion refuses blockers regardless of `--allow-lossy`, and
refuses to overwrite an existing output or report. Invalid packages fail validation
before producing a report or output. Treat reports' names and strings as
plain text, never HTML.

Library entry points are `Package::from_zip`, `Package::from_directory`, and
`Package::from_files` for browser/in-memory callers. Call `.validate()`, then
`.report()` and `.convert(Options { allow_lossy: true })`. The result contains
a `Model` and the compatibility report. Conversion passes through
`Model::from_clm_file`, so successful output satisfies the normal loader.

## Mapping and limitations

| Source | Current output |
| --- | --- |
| Canvas and layer boxes | Quad parts, centred on the canvas; Y reflected once; UVs unchanged |
| Layer order | Increasing z order; later layers draw in front |
| PNG | Original encoded bytes, straight alpha |
| WebP | Decoded and encoded as PNG at the importer boundary |
| Eye mask and iris clip | Invisible mask part, alpha threshold 0.5; soft animated clipping is not reproduced |
| Mouth frames | Separate parts, `mouthFrame/<layer-id>` selects the nearest integer frame; param name lists visemes |
| Expression patches | `expression/<id>` drives sampled smooth opacity; no temporal easing or coordinated brow replacement |
| Closed lids, blush, hole/edge helper layers | Hidden in the authored open-eye pose; no animation bindings |
| Author, license, generator, source extensions | Original manifest retained in the `livliv.import` extension |
| Required watermark | Conversion refused; metadata does not render a mark |
| Head turns, nod, roll, parallax, brows, blink, gaze, breathing | Not converted; flat authored geometry remains |
| Keyforms | Schema, group references and decoded field sizes validated; no displacement applied |
| Hair springs and independent chains | Not converted; no attempt to copy incompatible solver coefficients |
| Mouth loudness/vowel hysteresis, jaw and talking expressions | Not converted; select mouth drawings manually |
| Weighted show/hide/blush, expression brow textures | Not converted; named in per-layer losses |
| Avatar, cover, preview | Presence checked; not embedded as model textures |

Every successful model carries its source manifest, pinned upstream revision,
and loss report in `livliv.import`. Unsupported behavior is reported even
when an optional rig object is absent: the upstream player applies defaults.
A saved manifest preserves provenance; it does not implement its behaviors
or grant usage rights. Keyform data and unused media are not embedded.

The current adapter refuses v3 and newer major versions. Export v4 using the
upstream tools. Unknown required features and malformed schema fields are
errors. Future versions require a deliberate compatibility update.

## Resource boundaries

Packages are data only. The importer never executes data, extracts ZIPs to
disk or resolves a network URL. It accepts stored/DEFLATE ZIPs and a single
wrapping folder, ignoring platform metadata files. It refuses unsafe paths,
duplicate ZIP names, encrypted entries, symlinks, spanning and ZIP64.
Directory imports require a stable directory owned by the caller; concurrent
filesystem replacement during reading is not a supported security boundary.

Limits: 256 MiB encoded/uncompressed package, 2048 entries, 4 MiB manifest,
32 MiB keyforms, 512 layers, 8192 pixels per canvas side, and 512 MiB aggregate
decoded RGBA images. ZIP entry count is checked before parsing its central
directory. Every referenced image must match both its extension and layer
size. Alternate frames, masks and expression textures are checked too.

## Official sample validation

Use the pinned upstream checkout outside this repository. The `w01` sample
(Max) is covered by the LivLiv Featured Characters License 1.0, available in
that checkout's `characters/LICENSE.md`. It was made by LivLiv with AI.
Credit: **Characters: LivLiv (livliv.ai)**. The license permits local,
non-commercial testing but prohibits separate distribution of these assets,
including modified versions. Do not commit sample textures or converted
models as fixtures. The upstream code license does not license its models.

```sh
git clone https://github.com/livliv-dev/player.git /tmp/livliv-player
git -C /tmp/livliv-player checkout d999dea88981124168f9e9ab948eda8d2f23c453
cargo xtask import-livliv /tmp/livliv-player/characters/w01 --inspect \
  --report /tmp/livliv-w01-report.json
LIVLIV_SAMPLE_DIR=/tmp/livliv-player/characters/w01 \
  cargo test -p catchlight-importer official_sample -- --ignored
```

All official samples at this revision request a corner watermark. Inspection
is supported, but conversion must remain refused until output rendering can
honor the mark. Do not edit the manifest to bypass the check. A mark baked
into a model-space quad would move/crop with the camera and would not satisfy
the output-space requirement.

Automated tests use synthetic artwork created in memory. They check loader
round trips, reflected coordinates, preserved PNG/alpha/rights information,
actual evaluated mouth/expression opacity, package equivalence, malformed
references/resources, and watermark refusal. They do not establish visual
parity. Before claiming CL-14 complete, implement the remaining motion
mapping and compare rest, head turns, blink, gaze, expressions, mouth and
physics against the pinned player using an authorized sample.
