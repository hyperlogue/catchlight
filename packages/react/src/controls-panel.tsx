import type { ControlMappingInfo, ParamInfo, Session } from "@catchlight/core";
import { useState } from "react";

import type { ErrorHandler } from "./authoring.js";
import { EmptyState, IconButton, NumberField, round } from "./controls.js";
import {
  ExtensionWarnings,
  HUMANOID,
  NonstandardDialog,
  UnreadNotice,
  WarningMark,
  useExtensionWarnings,
  useHumanoid,
  useOfficialCommand,
  usePreviewInputs,
  warningsOn,
} from "./official.js";
import { useParams } from "./replica.js";

/** Headings in order, each with the name prefixes it collects. */
export const CONTROL_GROUPS: readonly (readonly [string, readonly string[]])[] = [
  ["Head", ["head_"]],
  ["Body", ["body_", "leg_"]],
  ["Eyes", ["eye_"]],
  ["Brows", ["brow_"]],
  ["Cheeks", ["cheek_", "blush"]],
  ["Mouth", ["mouth_", "teeth_", "lip_", "tongue_"]],
];

export interface ControlEntry {
  name: string;
  /** Runs 0..1 rather than -1..1. */
  oneSided: boolean;
  standard: boolean;
  mapping: ControlMappingInfo | undefined;
}

/**
 * The standard controls under their headings, in the standard's order, then
 * any mapped control outside the standard under Other.
 */
export function groupControls(
  controls: readonly { name: string; one_sided: boolean }[],
  mappings: readonly ControlMappingInfo[],
): { title: string; entries: ControlEntry[] }[] {
  const mapped = new Map(mappings.map((m) => [m.control, m]));
  const groups = CONTROL_GROUPS.map(([title]) => ({ title, entries: [] as ControlEntry[] }));
  const other: ControlEntry[] = [];
  for (const control of controls) {
    const entry = {
      name: control.name,
      oneSided: control.one_sided,
      standard: true,
      mapping: mapped.get(control.name),
    };
    const at = CONTROL_GROUPS.findIndex(([, prefixes]) =>
      prefixes.some((p) => control.name.startsWith(p)),
    );
    (at < 0 ? other : groups[at]!.entries).push(entry);
  }
  const standard = new Set(controls.map((c) => c.name));
  for (const m of mappings)
    if (!standard.has(m.control))
      other.push({ name: m.control, oneSided: false, standard: false, mapping: m });
  return [...groups, { title: "Other", entries: other }].filter((g) => g.entries.length);
}

/**
 * The humanoid controls: map each onto a param and preview it. Previewing is
 * per tab and authors nothing; mapping is an edit.
 */
export function ControlsPanel({ session, onError }: { session: Session; onError: ErrorHandler }) {
  const read = useHumanoid(session);
  const { warnings, params: warned } = useExtensionWarnings(session, HUMANOID);
  const params = useParams(session);
  const inputs = usePreviewInputs(session);
  const command = useOfficialCommand(session, onError);
  const [mappedOnly, setMappedOnly] = useState(false);
  // Names outside the standard the person chose to keep, sent as such from then on.
  const [kept, setKept] = useState<ReadonlySet<string>>(new Set());
  if (read.unread !== undefined)
    return (
      <div data-catchlight-controls-panel="">
        <UnreadNotice what="humanoid control mappings" why={read.unread} />
      </div>
    );
  const groups = groupControls(read.body.controls, read.body.mappings);
  const total = groups.reduce((n, g) => n + g.entries.length, 0);
  const showing = groups
    .map((g) => ({ ...g, entries: g.entries.filter((e) => !mappedOnly || e.mapping) }))
    .filter((g) => g.entries.length);
  const map = (control: string, param: string, stops?: [number, number, number]) =>
    void command.run({
      cmd: "humanoid_map",
      control,
      param,
      ...(stops ? { stops } : {}),
      nonstandard: kept.has(control),
    });
  return (
    <div data-catchlight-controls-panel="">
      <div data-catchlight-panel-tools="">
        <label data-catchlight-toggle="">
          <input
            type="checkbox"
            checked={mappedOnly}
            onChange={(e) => setMappedOnly(e.currentTarget.checked)}
          />
          Mapped only
        </label>
        <span data-catchlight-control-count="" role="status">
          {read.body.mappings.length} of {total} mapped
        </span>
      </div>
      <ExtensionWarnings warnings={warnings} />
      <div data-catchlight-control-cards="" aria-busy={command.busy || undefined}>
        {showing.map((group) => (
          <section key={group.title} data-catchlight-control-group="" aria-label={group.title}>
            <h3>{group.title}</h3>
            {group.entries.map((entry) => (
              <ControlCard
                key={entry.name}
                session={session}
                entry={entry}
                params={params}
                value={inputs.controls[entry.name]}
                warnings={entry.mapping ? warningsOn(warned, [entry.mapping.param]) : []}
                onMap={(param, stops) => map(entry.name, param, stops)}
                onUnmap={() => void command.run({ cmd: "humanoid_unmap", control: entry.name })}
              />
            ))}
          </section>
        ))}
      </div>
      {mappedOnly && !showing.length && (
        <EmptyState icon="link" title="No controls mapped yet">
          Pick a param for a control to map it, or show every control to see what is left.
        </EmptyState>
      )}
      <NonstandardDialog
        asking={command.asking}
        onCancel={command.dismiss}
        onKeep={() => {
          const asked = command.asking?.command;
          if (asked?.cmd === "humanoid_map") setKept((k) => new Set([...k, asked.control]));
          command.keep();
        }}
      />
    </div>
  );
}

function ControlCard({
  session,
  entry,
  params,
  value,
  warnings,
  onMap,
  onUnmap,
}: {
  session: Session;
  entry: ControlEntry;
  params: ParamInfo[];
  value: number | undefined;
  warnings: string[];
  onMap: (param: string, stops?: [number, number, number]) => void;
  onUnmap: () => void;
}) {
  const { name, mapping, oneSided } = entry;
  const stops = mapping?.stops;
  const missing = mapping && !params.some((p) => p.id === mapping.param);
  const setStop = (index: number, to: number) => {
    if (!mapping || !stops) return;
    const next: [number, number, number] = [...stops];
    next[index] = to;
    onMap(mapping.param, next);
  };
  return (
    <div
      data-catchlight-control-card=""
      data-control={name}
      data-mapped={mapping ? "" : undefined}
      data-previewing={value !== undefined ? "" : undefined}
      data-nonstandard={entry.standard ? undefined : ""}
      data-warned={warnings.length ? "" : undefined}
    >
      <div data-catchlight-control-head="">
        <span data-catchlight-control-name="" title={name}>
          {name}
        </span>
        <WarningMark warnings={warnings} />
        <select
          aria-label={`${name} param`}
          value={mapping?.param ?? ""}
          onChange={(e) => {
            const param = e.currentTarget.value;
            if (!param) onUnmap();
            else onMap(param);
          }}
        >
          <option value="">Unmapped</option>
          {missing && <option value={mapping.param}>{mapping.param} (removed)</option>}
          {params.map((p) => (
            <option key={p.id} value={p.id}>
              {p.name}
            </option>
          ))}
        </select>
      </div>
      <div data-catchlight-control-value-row="">
        <input
          type="range"
          data-catchlight-control-slider=""
          aria-label={`Preview ${name}`}
          min={oneSided ? 0 : -1}
          max={1}
          step={0.01}
          value={value ?? 0}
          disabled={!mapping}
          onInput={(e) => session.setControl(name, Number(e.currentTarget.value))}
          onChange={noop}
        />
        <output data-catchlight-control-value="" aria-label={`${name} preview value`}>
          {value === undefined ? "–" : round(value).toFixed(2)}
        </output>
        <IconButton
          icon="close"
          label={`Clear ${name} preview`}
          disabled={value === undefined}
          onClick={() => session.setControl(name, undefined)}
        />
      </div>
      {stops && (
        <div data-catchlight-control-stops="">
          {(["−1", "0", "+1"] as const).map((at, i) => (
            <NumberField
              key={at}
              label={`${name} stop at ${at}`}
              prefix={at}
              value={stops[i]!}
              onCommit={(v) => setStop(i, v)}
            />
          ))}
        </div>
      )}
    </div>
  );
}

// React requires a controlled input's `onChange`; the native input event is
// handled once, through `onInput`.
function noop(): void {}
