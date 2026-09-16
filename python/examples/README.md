# Reproducible rig review

`rig_workflow.py` generates a small textured triangle and two independent
deformation bindings. It needs no private model, image files or Python packages
beyond the Catchlight SDK. Run from the repository root:

```sh
nix develop -c cargo build -p catchlight-editor-server -p catchlight-cli
nix develop -c env PYTHONPATH=python python python/examples/rig_workflow.py \
  --cli target/debug/catchlight-cli --out /tmp/catchlight-rig-review
```

Use a fresh output directory. `--server PATH` optionally selects the editor
server binary. The Nix shell supplies lavapipe when no hardware GPU is present.

The example composes ordinary operations:

1. Create a synthetic model with binding-owned grids, an unset middle cell,
   texture, named vertex slots and a weld. Capture its revision and fork it.
2. Read mesh and authored cells at that revision. Choose a cut through two
   triangle edges, map the new Part's offsets in client code, and supply exact
   `deform_mapping` weights for the retained Part. Refill its slots and add seam
   welds in the same atomic edit. Authored holes stay unset.
3. Move a shared affine X deformation into a MeshGroup. The remaining local
   shape changes only Y, so these particular fields compose exactly. The
   example does not infer an exact transfer for arbitrary rigs; topology,
   representability and any fitting policy belong to the caller.
4. Compare geometry at five chosen poses, export baseline/cut/assembled CLMs,
   and render each with the independent file CLI. Inspect each `run.json`, PNG
   and geometry sidecar. `edits.json` contains the explicit replayable batch.
5. Replay that batch on the source at the captured revision. Its exported
   bytes must equal the reviewed candidate. Demonstrate that a second replay
   with the old guard is refused, then close the synthetic sessions.

The client's acceptance policy is an absolute geometry tolerance of `1e-5`
world units at those poses plus successful rendering. Pixel comparison and
broader pose selection can be added by the caller. `report.json` records the
result without filesystem paths or machine identity.

`python/tests/test_rig_workflow.py` tests the same recipes without a GPU. It
checks sparse cells, binding grids, slots, weld order, geometry, whole-batch
rollback, exact candidate replay, stale guards and one-step Undo restoration.

```sh
nix develop -c uv run --project python pytest python/tests/test_rig_workflow.py -q
```
