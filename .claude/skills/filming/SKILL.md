---
name: filming
description: Film a Catchlight model by rendering shots as lossless passes with catchlight-cli and compositing them into a video. Use when making or revising a film, reel or demo video of a model.
---

# Filming

A film is a **timeline** (one value per film frame for every animated param)
cut into **shots** (a window of the timeline at one framing). Each shot renders
as one or more **passes** (the same motion with different parts, backgrounds or
sidecars), and a compositing function turns passes into film frames. Iterate at
**preview** quality (every 4th frame, half scale); render **final** once the
cut is approved.

The scripts in `scripts/` are film-agnostic; a film's own code (timeline,
scenes, graphics, audio) lives in the film's working directory, outside this
repository, and imports them. Run everything in the dev shell, which supplies
the GPU stack, ffmpeg and numpy.

`scripts/example.py <model.clm> --param <id>` is the smallest whole film and
the skeleton to copy.

## Steps

1. **Timeline.** Sample every animated param at the film's fps for the whole
   film. Seed any randomness (blinks, idle motion) from stable ids such as a
   segment's name, so a revision changes only the frames it meant to and
   `check_cut.py --against` stays readable. Done when each track has one value
   per film frame.
2. **Shots.** Build specs with `shots.shot` and store them with `shots.write`,
   one directory per quality. `catchlight-cli bounds` gives framing rects and
   part ids; `catchlight-cli render --schema spec` documents every field a
   pass may carry (parts, masks, camera keys, overlays, geometry, traces).
   Done when `catchlight-cli render <model> --spec <file> --validate` accepts
   every spec at both qualities: a final spec carries four times the preview's
   frames at twice the scale, so it can exceed a budget the preview passes.
3. **Render.** `scripts/render.py <model> <spec-dir> <out-dir> [--pack]`.
   Done when every line reads `ok`; a `FAIL` line carries the CLI's last
   message and the full log is `<out-dir>/<name>.log`.
4. **Compose.** Write `frame(i)` over `passes.Shots` and encode it with
   `compose.encode`. Check stills of every scene at preview before encoding a
   whole film. A half-scale preview hides texture faults: for each close-up,
   also render a few final-scale stills of the mouth and eyes, open and
   closed, before the cut is approved. Done when the master exists and step 5
   passes.
5. **Check.** `scripts/check_cut.py <cut> --frames N --seconds S`, adding
   `--audio` after `compose.mux` and `--against <previous cut>` on a revision.
   Then watch the cut, or extract frames and look at them, at every scene
   boundary. Done when the counts match and the worst PSNR frame is explained.

## Shots and passes

- Passes of one shot share its animation, so motion responses match frame for
  frame across them. A part filmed alone belongs in a pass of the same shot.
- Motion responses simulate from the shot's first frame. Give `pre` enough
  lead time for hair and cloth to be moving as they would mid-film, and start
  `t0` a margin before the cut so transitions have frames on both sides.
- Passes render over transparent black. Multiply-blended parts vanish there:
  give their pass `shots.WHITE` as its background and read it back with
  `passes.from_white`.
- `split=True` gives each pass its own CLI run. Use it when a shot exceeds the
  CLI's pixel budget, which `--validate` reports by name.
- Each pass is one lossless FFV1 video, `<pass>.mkv`: bit-exact RGBA, far
  faster to write than PNG frames. Geometry and trace sidecars are per frame;
  `--pack` folds them into one `.npz` per request.
- The CLI encodes with `CATCHLIGHT_FFMPEG`, else `ffmpeg` on PATH, and fails
  before rendering when neither runs.

## Compositing

- `passes.Shots.frame(shot, pass, t)` returns straight-alpha RGBA at film time
  `t`; `passes.premultiply` and `passes.over` blend it onto an RGB canvas.
- Each worker process holds one ffmpeg decoder per pass of the scene it is
  compositing, a few hundred MB each at film resolution, plus its own caches.
  Memory therefore scales with `jobs` times the passes in the busiest scene.
  Start `compose.encode` at `jobs=2`; raise it after measuring one process on
  that scene.
- Composite at the render's quality: a preview pass is half scale, so the
  preview film is too.

## Budgets

- The CLI caps each spec's pixels and geometry triangles, and `--validate`
  names the one exceeded. Triangles count every listed part on every frame,
  so give a `geometry` pass only the parts the shot draws from it.
- Final passes take roughly ten times the preview's disk. Check free space
  before a final render; a full disk truncates files silently.
- `render.py` replaces each shot's output directory as it starts. Keep the
  previous cut's delivery file until the new one passes step 5.
- Review hosts and social platforms cap upload size. Keep the master and
  re-encode a smaller copy for them; run `check_cut.py` on both.
