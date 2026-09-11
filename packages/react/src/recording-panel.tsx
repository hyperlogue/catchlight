/** Recording controls share one explicit destination with canvas gestures.
 * Hollow keypoints are derived; clicking or scrubbing a param never authors. */
import { useRef } from "react";
import { useEditing } from "./editing.js";
import {
  Disclosure,
  EmptyState,
  Field,
  Icon,
  IconButton,
  NumberField,
} from "./controls.js";
import { ParamSliderRoot } from "./param-slider.js";
import { valueAtKey } from "./bindings.js";

const display = (n: number) =>
  new Intl.NumberFormat("en", {
    maximumFractionDigits: 5,
    signDisplay: "exceptZero",
  }).format(n);

export function WorkspaceModes() {
  const edit = useEditing()!;
  const refs = useRef<(HTMLButtonElement | null)[]>([]);
  return (
    <div
      data-catchlight-workspace-modes=""
      role="group"
      aria-label="Editor workspace"
    >
      {(
        [
          ["arrange", "select", "Arrange"],
          ["mesh", "mesh", "Mesh"],
          ["record", "key", "Record"],
        ] as const
      ).map(([mode, icon, label], index) => (
        <button
          ref={(el) => {
            refs.current[index] = el;
          }}
          key={mode}
          type="button"
          aria-pressed={edit.mode === mode}
          title={
            mode === "mesh"
              ? "Edit mesh on artwork (M)"
              : mode === "record"
                ? "Record parameter keypoints (R)"
                : "Arrange the model"
          }
          data-recording={mode === "record" && edit.recording ? "" : undefined}
          disabled={
            !edit.session ||
            edit.busy ||
            (mode === "mesh" &&
              edit.mode !== "mesh" &&
              (edit.info?.kind !== "part" || !edit.info.texture))
          }
          onClick={() => edit.requestMode(mode)}
          onKeyDown={(e) => {
            if (e.key !== "ArrowLeft" && e.key !== "ArrowRight") return;
            e.preventDefault();
            const direction = e.key === "ArrowRight" ? 1 : -1;
            for (let step = 1; step < 4; step++) {
              const next = refs.current[(index + step * direction + 6) % 3];
              if (next && !next.disabled) {
                next.focus();
                next.click();
                break;
              }
            }
          }}
        >
          <Icon name={icon} width="14" height="14" />
          {label}
          {mode === "record" && edit.recording && (
            <i data-catchlight-record-dot="" />
          )}
        </button>
      ))}
    </div>
  );
}

export function EditingBar() {
  const edit = useEditing()!;
  if (edit.mode === "arrange") return null;
  return (
    <div
      data-catchlight-editing-bar=""
      data-mode={edit.mode}
      data-recording={edit.recording ? "" : undefined}
    >
      <div data-catchlight-editing-title="">
        <Icon name={edit.mode === "mesh" ? "mesh" : "key"} />
        <div>
          <strong>
            {edit.mode === "mesh"
              ? "Edit mesh"
              : edit.recording
                ? "Recording keypoint"
                : "Record a keypoint"}
            <span> · {edit.info?.name ?? "Choose a node"}</span>
          </strong>
          <small>
            {edit.mode === "mesh"
              ? "Original artwork · texture stays fixed"
              : edit.primary
                ? `${edit.primary.name} ${display(edit.xValue)}${edit.secondary ? ` × ${edit.secondary.name} ${display(edit.yValue)}` : ""} · ${edit.recordTool === "shape" ? "Mesh shape" : "Transform & appearance"}`
                : "Choose a param to define the recording destination"}
          </small>
        </div>
      </div>
      <span data-catchlight-editing-state="">
        {edit.mode === "mesh"
          ? edit.mesh?.dirty
            ? "Draft modified"
            : "Mesh draft"
          : "Physics paused"}
      </span>
      {edit.mode === "mesh" ? (
        <div data-catchlight-editing-actions="">
          <button type="button" disabled={edit.busy} onClick={edit.discardMesh}>
            Cancel
          </button>
          <button
            type="button"
            data-primary=""
            disabled={edit.busy || (edit.stale && !!edit.mesh?.dirty)}
            onClick={() => void edit.applyMesh()}
          >
            {edit.busy ? "Applying…" : edit.mesh?.dirty ? "Apply mesh" : "Done"}
          </button>
        </div>
      ) : (
        <RecordButton />
      )}
    </div>
  );
}
function RecordButton() {
  const edit = useEditing()!;
  return (
    <button
      type="button"
      data-catchlight-record-button=""
      data-recording={edit.recording ? "" : undefined}
      disabled={
        edit.busy ||
        !edit.info ||
        !edit.primary ||
        (edit.recordTool === "shape" && !edit.info.vertex_count)
      }
      onClick={edit.recording ? edit.stop : edit.arm}
    >
      <i data-catchlight-record-dot="" />
      {edit.busy
        ? "Saving keypoint…"
        : edit.recording
          ? "Stop recording"
          : "Start recording"}
    </button>
  );
}

export function RecordingInspector() {
  const edit = useEditing()!;
  if (!edit.info)
    return (
      <EmptyState icon="key" title="Choose what to record">
        Select a part or group in the tree. Then choose the param and keypoint
        it should respond to.
      </EmptyState>
    );
  const selectedBindings = edit.target
    ? edit.relevant.filter(
        (b) => b.authored[edit.target!.cell[1]]?.[edit.target!.cell[0]],
      )
    : [];
  return (
    <div
      data-catchlight-recording-inspector=""
      data-recording={edit.recording ? "" : undefined}
    >
      <div data-catchlight-selection-heading="">
        <Icon name={edit.recordTool === "shape" ? "mesh" : "key"} />
        <div>
          <strong>{edit.info.name}</strong>
          <small>Recording destination</small>
        </div>
      </div>
      <div
        data-catchlight-record-scope=""
        role="group"
        aria-label="Record properties"
      >
        <button
          type="button"
          aria-pressed={edit.recordTool === "shape"}
          disabled={edit.busy || !edit.info.vertex_count}
          onClick={() => edit.setRecordTool("shape")}
        >
          <Icon name="mesh" width="14" height="14" />
          Mesh shape
        </button>
        <button
          type="button"
          aria-pressed={edit.recordTool === "transform"}
          disabled={edit.busy}
          onClick={() => edit.setRecordTool("transform")}
        >
          <Icon name="select" width="14" height="14" />
          Transform
        </button>
      </div>
      <Field label="Driven by">
        <select
          aria-label="Recording param"
          value={edit.param ?? ""}
          disabled={edit.busy}
          onChange={(e) => edit.selectParam(e.currentTarget.value)}
        >
          {!edit.params.length && (
            <option value="">Create a param below</option>
          )}
          {edit.params.map((p) => (
            <option key={p.id} value={p.id}>
              {p.name}
            </option>
          ))}
        </select>
      </Field>
      {edit.secondary && (
        <div data-catchlight-pair-note="">
          <Icon name="link" width="13" height="13" />
          <span>
            Paired with{" "}
            <b>
              {edit.param === edit.secondary.id
                ? edit.primary?.name
                : edit.secondary.name}
            </b>
          </span>
        </div>
      )}
      {edit.pairChoices.length > 1 && (
        <Field label="Param pair">
          <select
            aria-label="Recording param pair"
            value={edit.pair?.join("|")}
            onChange={(e) =>
              edit.setPair(e.currentTarget.value.split("|") as [string, string])
            }
          >
            {edit.pairChoices.map((pair) => (
              <option key={pair.join("|")} value={pair.join("|")}>
                {pair
                  .map((id) => edit.params.find((p) => p.id === id)?.name ?? id)
                  .join(" × ")}
              </option>
            ))}
          </select>
        </Field>
      )}
      <div data-catchlight-record-target="">
        <small>{edit.recording ? "Recording here" : "Selected keypoint"}</small>
        <strong>
          {edit.recordTool === "shape"
            ? "Mesh shape"
            : "Transform & appearance"}
        </strong>
        <p>
          {edit.primary
            ? `${edit.primary.name} ${display(edit.xValue)}`
            : "No param selected"}
        </p>
        {edit.secondary && (
          <p>
            {edit.secondary.name} {display(edit.yValue)}
          </p>
        )}
        <span data-catchlight-key-state="">
          <Icon
            name="key"
            width="12"
            height="12"
            data-authored={edit.authored ? "" : undefined}
          />
          {!edit.target
            ? "Between key positions"
            : edit.authored
              ? edit.recordTool === "shape"
                ? "Authored keypoint"
                : `${selectedBindings.length} properties keyed`
              : "Derived · no keypoint authored here"}
        </span>
      </div>
      <p data-catchlight-hint="">
        {edit.recording
          ? "Each completed gesture records this keypoint. Escape cancels the current drag."
          : "Start recording to capture edits at this keypoint. Your first edit creates the keypoint."}
      </p>
      {edit.recordTool === "shape" ? (
        <>
          <div data-catchlight-record-instructions="">
            <Icon name="select" />
            <p>
              Drag a vertex on the canvas to shape the artwork. The base mesh
              stays fixed.
            </p>
          </div>
          <Disclosure title="Keypoint actions" defaultOpen={false}>
            <button
              type="button"
              data-catchlight-action=""
              disabled={!edit.target || edit.busy}
              onClick={() => void edit.keyAction("binding_reset")}
            >
              <Icon name="reset" />
              Set neutral shape
            </button>
            <button
              type="button"
              data-catchlight-action=""
              disabled={!edit.target || !edit.authored || edit.busy}
              onClick={() => void edit.keyAction("binding_unset")}
            >
              <Icon name="minus" />
              Clear authored keypoint
            </button>
            <p data-catchlight-hint="">
              Neutral records zero offsets. Clear returns this cell to
              interpolation.
            </p>
          </Disclosure>
        </>
      ) : (
        <RecordedFields />
      )}
      <div data-catchlight-edit-footer="">
        <p>
          Changing the selection or scrubbing a param stops recording. Stop
          recording keeps the keypoints you have already made.
        </p>
      </div>
      <button
        type="button"
        data-catchlight-action=""
        disabled={edit.busy || edit.info.kind !== "part" || !edit.info.texture}
        onClick={() => edit.requestMode("mesh")}
      >
        <Icon name="mesh" />
        Edit base mesh…
      </button>
    </div>
  );
}
function RecordedFields() {
  const edit = useEditing()!,
    p = edit.posed;
  if (!p)
    return (
      <p data-catchlight-hint="">
        Choose a key position to inspect and edit its posed values.
      </p>
    );
  return (
    <fieldset
      disabled={!edit.recording || edit.busy}
      data-catchlight-recorded-fields=""
    >
      <Disclosure title="Transform">
        <Field label="Position">
          {([0, 1] as const).map((i) => (
            <NumberField
              key={i}
              label={`Recorded position ${i === 0 ? "X" : "Y"}`}
              unit={i === 0 ? "X" : "Y"}
              value={p.translate?.[i] ?? 0}
              onCommit={(v) => {
                const translate: [number, number, number] = [
                  ...(p.translate ?? [0, 0, 0]),
                ];
                translate[i] = v;
                void edit.patch({ translate });
              }}
            />
          ))}
        </Field>
        <Field label="Rotation">
          <NumberField
            label="Recorded rotation"
            value={((p.rotate?.[2] ?? 0) * 180) / Math.PI}
            unit="°"
            onCommit={(v) =>
              void edit.patch({
                rotate: [
                  p.rotate?.[0] ?? 0,
                  p.rotate?.[1] ?? 0,
                  (v * Math.PI) / 180,
                ],
              })
            }
          />
        </Field>
        <Field label="Scale">
          {([0, 1] as const).map((i) => (
            <NumberField
              key={i}
              label={`Recorded scale ${i === 0 ? "X" : "Y"}`}
              unit={i === 0 ? "X" : "Y"}
              value={p.scale?.[i] ?? 1}
              onCommit={(v) => {
                const scale: [number, number] = [...(p.scale ?? [1, 1])];
                scale[i] = v;
                void edit.patch({ scale });
              }}
            />
          ))}
        </Field>
        <Field label="Draw order">
          <NumberField
            label="Recorded draw order"
            value={p.z_order ?? 0}
            onCommit={(v) => void edit.patch({ z_order: v })}
          />
        </Field>
      </Disclosure>
      {p.opacity != null && (
        <Disclosure title="Appearance" defaultOpen={false}>
          <Field label="Opacity">
            <NumberField
              label="Recorded opacity"
              value={p.opacity * 100}
              min={0}
              max={100}
              unit="%"
              onCommit={(v) => void edit.patch({ opacity: v / 100 })}
            />
          </Field>
          {(
            [
              ["tint", "Tint"],
              ["screen_tint", "Screen"],
            ] as const
          ).map(([key, label]) => (
            <Field key={key} label={label}>
              {([0, 1, 2] as const).map((i) => (
                <NumberField
                  key={i}
                  label={`Recorded ${label} ${["red", "green", "blue"][i]}`}
                  value={p[key]?.[i] ?? 0}
                  min={0}
                  unit={(["R", "G", "B"] as const)[i]}
                  onCommit={(v) => {
                    const color: [number, number, number] = [
                      ...(p[key] ?? [0, 0, 0]),
                    ];
                    color[i] = v;
                    void edit.patch({ [key]: color });
                  }}
                />
              ))}
            </Field>
          ))}
        </Disclosure>
      )}
    </fieldset>
  );
}

export function RecordingKeys() {
  const edit = useEditing()!,
    { primary, secondary, session } = edit;
  if (!session || !primary)
    return (
      <EmptyState icon="key" title="Create a param to start recording">
        Params connect your artwork to movement. Add one in the Params tab.
      </EmptyState>
    );
  const keyState = (x: number, y: number) =>
    edit.relevant.some((b) => b.authored[y]?.[x]);
  const keyButton = (x: number, y: number, matrix = false) => {
    const authored = keyState(x, y),
      selected = edit.x === x && edit.y === y;
    const label = `${primary.name} ${display(valueAtKey(primary, x))}${secondary ? `, ${secondary.name} ${display(valueAtKey(secondary, y))}` : ""}`;
    return (
      <button
        type="button"
        key={`${x}:${y}`}
        aria-label={`${label}, ${authored ? "authored" : "derived"} keypoint`}
        aria-pressed={selected}
        data-catchlight-record-key=""
        data-key-x={x}
        data-key-y={y}
        data-authored={authored ? "" : undefined}
        data-recording={selected && edit.recording ? "" : undefined}
        disabled={edit.busy}
        title={label}
        onClick={() => edit.selectCell(x, y)}
      >
        <Icon name="key" width="13" height="13" />
        {!matrix && (
          <>
            <span>{display(valueAtKey(primary, x))}</span>
            <small>{authored ? "Authored" : "Derived"}</small>
          </>
        )}
      </button>
    );
  };
  return (
    <div
      data-catchlight-recording-keys=""
      data-paired={secondary ? "" : undefined}
    >
      <div data-catchlight-key-shelf-heading="">
        <span>
          <b>{edit.info?.name ?? "Selection"}</b> /{" "}
          {edit.recordTool === "shape"
            ? "Mesh shape"
            : "Transform & appearance"}
        </span>
        <small>◇ derived &nbsp; ◆ authored</small>
      </div>
      {edit.gate && !edit.target && (
        <div data-catchlight-key-gate="" role="status">
          <div>
            <strong>This pose is between key positions</strong>
            <p>
              Snap to an existing keypoint, or add a position at the current
              value{secondary ? "s" : ""}.
            </p>
          </div>
          <div>
            <button
              type="button"
              title="Snap to the nearest existing keypoint and start recording"
              disabled={edit.busy}
              onClick={edit.snap}
            >
              Snap & record
            </button>
            <button
              type="button"
              data-primary=""
              disabled={edit.busy}
              onClick={() => void edit.insert()}
            >
              Add position & record
            </button>
          </div>
          <small>
            New positions expand the grid for every binding driven by{" "}
            {secondary ? "these params" : "this param"}.
          </small>
        </div>
      )}
      <div data-catchlight-key-shelf-content="">
        <div data-catchlight-record-sliders="">
          {[primary, ...(secondary ? [secondary] : [])].map((p) => (
            <div key={p.id} data-catchlight-record-slider="">
              <label>{p.name}</label>
              <ParamSliderRoot
                session={session}
                param={p}
                disabled={edit.busy}
              />
              <NumberField
                label={`${p.name} recording value`}
                value={session.paramValue(p.id) ?? p.default}
                min={p.min}
                max={p.max}
                disabled={edit.busy}
                onCommit={(v) => session.setParam(p.id, v)}
              />
            </div>
          ))}
          {!secondary && (
            <div data-catchlight-record-key-strip="">
              {primary.key_positions.map((_, x) => keyButton(x, 0))}
            </div>
          )}
          <p data-catchlight-record-pose-hint="">
            {edit.recording
              ? "Recording is on. Scrubbing stops capture; completed edits are kept."
              : "Scrub to preview. Select a keypoint, then start recording to edit it."}
          </p>
        </div>
        {secondary && (
          <div data-catchlight-record-matrix-scroll="">
            <div data-catchlight-record-matrix-label="">
              <span>{secondary.name} ↑</span>
              <span>{primary.name} →</span>
            </div>
            <div
              data-catchlight-record-matrix=""
              style={{
                gridTemplateColumns: `42px repeat(${primary.key_positions.length}, minmax(34px, 1fr))`,
              }}
            >
              <span />
              {primary.key_positions.map((_, x) => (
                <span key={x}>{display(valueAtKey(primary, x))}</span>
              ))}
              {secondary.key_positions
                .map((_, y) => secondary.key_positions.length - 1 - y)
                .map((y) => (
                  <div key={y} data-catchlight-matrix-row="">
                    <span>{display(valueAtKey(secondary, y))}</span>
                    {primary.key_positions.map((_, x) => keyButton(x, y, true))}
                  </div>
                ))}
            </div>
          </div>
        )}
      </div>
    </div>
  );
}
