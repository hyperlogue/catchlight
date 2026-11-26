import type {
  ExpressionGroup,
  ExpressionInfo,
  ExpressionSetting,
  ParamId,
  ParamInfo,
  Session,
} from "@catchlight/core";
import { useRef, useState } from "react";

import type { ErrorHandler } from "./authoring.js";
import { Icon, IconButton, Modal, NumberField, round } from "./controls.js";
import {
  EXPRESSIONS,
  ExtensionWarnings,
  NonstandardDialog,
  UnreadNotice,
  WarningMark,
  plural,
  useExpressions,
  useExtensionWarnings,
  useHumanoid,
  useOfficialCommand,
  useLiveInputs,
  warningsOn,
} from "./official.js";
import { useParams } from "./replica.js";

const NOUN: Record<ExpressionGroup, string> = { expression: "expression", viseme: "viseme" };

interface Draft {
  group: ExpressionGroup;
  name: string;
  isNew: boolean;
  settings: ExpressionSetting[];
}

/** A control's range: the standard control's own, or -1..1 for another name. */
type ControlRanges = ReadonlyMap<string, readonly [number, number]>;

/**
 * The model's expressions and visemes: try each at a live weight, and author
 * them as lists of settings. A live weight is per tab and authors nothing.
 */
export function ExpressionsPanel({ session, onError }: { session: Session; onError: ErrorHandler }) {
  const read = useExpressions(session);
  const humanoid = useHumanoid(session);
  const { warnings, params: warned } = useExtensionWarnings(session, EXPRESSIONS);
  const inputs = useLiveInputs(session);
  const command = useOfficialCommand(session, onError);
  const [naming, setNaming] = useState<ExpressionGroup>();
  const [draft, setDraft] = useState<Draft>();
  const [askStandard, setAskStandard] = useState(false);
  const [report, setReport] = useState<string>();
  if (read.unread !== undefined)
    return (
      <div data-catchlight-expressions-panel="">
        <UnreadNotice what="expressions and visemes" why={read.unread} />
      </div>
    );
  const { expressions, visemes, emotion_names, viseme_names } = read.body;
  const ranges: ControlRanges = new Map(
    (humanoid.body?.controls ?? []).map((c) => [c.name, [c.one_sided ? 0 : -1, 1] as const]),
  );
  const addStandard = (replace: boolean) => {
    setAskStandard(false);
    void command.run({ cmd: "viseme_add_standard", replace }).then((body) => {
      if (body?.result !== "visemes_added") return;
      setReport(
        `Standard visemes: ${body.added.length} added, ${body.replaced.length} replaced, ${body.skipped.length} skipped.`,
      );
    });
  };
  const list = (group: ExpressionGroup, members: ExpressionInfo[], weights: Record<string, number>) => (
    <ExpressionList
      group={group}
      members={members}
      weights={weights}
      warned={warned}
      onWeight={(name, weight) =>
        group === "viseme" ? session.setVisemeWeight(name, weight) : session.setExpressionWeight(name, weight)
      }
      onEdit={(member) => setDraft({ group, name: member.name, isNew: false, settings: member.settings })}
      onDelete={(member) =>
        void command.run({ cmd: "expression_delete", group, name: member.name }, () =>
          group === "viseme"
            ? session.setVisemeWeight(member.name, undefined)
            : session.setExpressionWeight(member.name, undefined),
        )
      }
    />
  );
  return (
    <div data-catchlight-expressions-panel="">
      <ExtensionWarnings warnings={warnings} />
      <div data-catchlight-expression-groups="">
        <section data-catchlight-expression-group="expression" aria-label="Expressions">
          <header>
            <h3>Expressions</h3>
            <button type="button" onClick={() => setNaming("expression")}>
              <Icon name="plus" width="13" height="13" />
              Add expression
            </button>
          </header>
          {list("expression", expressions, inputs.expressions)}
          {!expressions.length && (
            <p data-catchlight-hint="">
              No expressions yet. An emotion such as happy means the same on every model.
            </p>
          )}
        </section>
        <section data-catchlight-expression-group="viseme" aria-label="Visemes">
          <header>
            <h3>Visemes</h3>
            <button
              type="button"
              data-catchlight-viseme-standard=""
              disabled={command.busy}
              onClick={() => (visemes.length ? setAskStandard(true) : addStandard(false))}
            >
              Add standard visemes
            </button>
            <button type="button" onClick={() => setNaming("viseme")}>
              <Icon name="plus" width="13" height="13" />
              Add viseme
            </button>
          </header>
          {report && (
            <p data-catchlight-hint="" role="status" data-catchlight-viseme-report="">
              {report}
            </p>
          )}
          {list("viseme", visemes, inputs.visemes)}
          {!visemes.length && (
            <p data-catchlight-hint="">
              No visemes yet. Lip sync fades between them, over the expressions.
            </p>
          )}
        </section>
      </div>
      {naming && (
        <NameDialog
          group={naming}
          standard={(naming === "viseme" ? viseme_names : emotion_names).filter(
            (n) => !(naming === "viseme" ? visemes : expressions).some((m) => m.name === n),
          )}
          taken={(naming === "viseme" ? visemes : expressions).map((m) => m.name)}
          onCancel={() => setNaming(undefined)}
          onName={(name) => {
            setNaming(undefined);
            setDraft({ group: naming, name, isNew: true, settings: [] });
          }}
        />
      )}
      {draft && (
        <ExpressionEditor
          key={`${draft.group}:${draft.name}`}
          session={session}
          draft={draft}
          ranges={ranges}
          busy={command.busy}
          onCancel={() => setDraft(undefined)}
          onSave={(settings) =>
            void command.run(
              { cmd: "expression_set", group: draft.group, name: draft.name, settings, nonstandard: false },
              () => setDraft(undefined),
            )
          }
        />
      )}
      {askStandard && (
        <Modal title="Add the standard visemes" onClose={() => setAskStandard(false)}>
          <p data-catchlight-hint="">
            This model already has {plural(visemes.length, "viseme")}. Keep the ones that share a
            standard name, or replace them with the standard settings? Every missing standard viseme
            is added either way.
          </p>
          <div data-catchlight-dialog-actions="">
            <button type="button" onClick={() => setAskStandard(false)}>
              Cancel
            </button>
            <button type="button" data-catchlight-danger="" onClick={() => addStandard(true)}>
              Replace them
            </button>
            <button type="button" data-primary="" onClick={() => addStandard(false)}>
              Keep mine
            </button>
          </div>
        </Modal>
      )}
      <NonstandardDialog asking={command.asking} onCancel={command.dismiss} onKeep={command.keep} />
    </div>
  );
}

function ExpressionList({
  group,
  members,
  weights,
  warned,
  onWeight,
  onEdit,
  onDelete,
}: {
  group: ExpressionGroup;
  members: ExpressionInfo[];
  weights: Record<string, number>;
  warned: Map<ParamId, string[]>;
  onWeight: (name: string, weight: number | undefined) => void;
  onEdit: (member: ExpressionInfo) => void;
  onDelete: (member: ExpressionInfo) => void;
}) {
  return (
    <>
      {members.map((member) => {
        const weight = weights[member.name];
        const warnings = warningsOn(
          warned,
          member.settings.flatMap((s) => (s.param != null ? [s.param] : [])),
        );
        return (
          <div
            key={member.name}
            data-catchlight-expression-row=""
            data-group={group}
            data-name={member.name}
            data-live={weight !== undefined ? "" : undefined}
            data-warned={warnings.length ? "" : undefined}
          >
            <span data-catchlight-expression-name="" title={member.name}>
              {member.name}
            </span>
            <WarningMark warnings={warnings} />
            <input
              type="range"
              data-catchlight-expression-slider=""
              aria-label={`${member.name} weight`}
              min={0}
              max={1}
              step={0.01}
              value={weight ?? 0}
              onInput={(e) => {
                const value = Number(e.currentTarget.value);
                onWeight(member.name, value === 0 ? undefined : value);
              }}
              onChange={noop}
            />
            <output data-catchlight-expression-weight="">{round(weight ?? 0).toFixed(2)}</output>
            <small data-catchlight-expression-count="">{plural(member.settings.length, "setting")}</small>
            <IconButton icon="settings" label={`Edit ${member.name}`} onClick={() => onEdit(member)} />
            <IconButton icon="trash" label={`Delete ${member.name}`} onClick={() => onDelete(member)} />
          </div>
        );
      })}
    </>
  );
}

function NameDialog({
  group,
  standard,
  taken,
  onCancel,
  onName,
}: {
  group: ExpressionGroup;
  standard: string[];
  taken: string[];
  onCancel: () => void;
  onName: (name: string) => void;
}) {
  const [name, setName] = useState("");
  const trimmed = name.trim();
  const exists = taken.includes(trimmed);
  return (
    <Modal title={`New ${NOUN[group]}`} onClose={onCancel}>
      <form
        onSubmit={(e) => {
          e.preventDefault();
          if (trimmed && !exists) onName(trimmed);
        }}
      >
        {standard.length > 0 && (
          <div data-catchlight-name-choices="" role="group" aria-label="Standard names">
            <span data-catchlight-field-label="">Standard names</span>
            <div>
              {standard.map((n) => (
                <button
                  type="button"
                  key={n}
                  aria-pressed={trimmed === n}
                  data-active={trimmed === n ? "" : undefined}
                  onClick={() => setName(n)}
                >
                  {n}
                </button>
              ))}
            </div>
          </div>
        )}
        <label data-catchlight-form-label="">
          Name
          <input
            autoFocus
            required
            placeholder={group === "viseme" ? "e.g. aa" : "e.g. happy"}
            value={name}
            onChange={(e) => setName(e.currentTarget.value)}
          />
        </label>
        <p data-catchlight-hint="">
          {group === "viseme"
            ? "A standard name lip sync knows, or one of your own."
            : "A standard emotion means the same on every model; any other name is your own."}
        </p>
        {exists && (
          <p role="alert" data-catchlight-warning="">
            There is already {group === "viseme" ? "a viseme" : "an expression"} named “{trimmed}”.
            Edit it from its row.
          </p>
        )}
        <div data-catchlight-dialog-actions="">
          <button type="button" onClick={onCancel}>
            Cancel
          </button>
          <button type="submit" data-primary="" disabled={!trimmed || exists}>
            Continue
          </button>
        </div>
      </form>
    </Modal>
  );
}

interface Row {
  key: number;
  setting: ExpressionSetting;
}

function ExpressionEditor({
  session,
  draft,
  ranges,
  busy,
  onCancel,
  onSave,
}: {
  session: Session;
  draft: Draft;
  ranges: ControlRanges;
  busy: boolean;
  onCancel: () => void;
  onSave: (settings: ExpressionSetting[]) => void;
}) {
  const params = useParams(session);
  const next = useRef(0);
  const [rows, setRows] = useState<Row[]>(() =>
    draft.settings.map((setting) => ({ key: next.current++, setting })),
  );
  // Names outside the standard that the settings already use stay choosable.
  const controls = [
    ...ranges.keys(),
    ...rows.flatMap((r) => (r.setting.control != null && !ranges.has(r.setting.control) ? [r.setting.control] : [])),
  ];
  const range = (control: string): readonly [number, number] => ranges.get(control) ?? [-1, 1];
  const update = (key: number, setting: ExpressionSetting) =>
    setRows((all) => all.map((r) => (r.key === key ? { key, setting } : r)));
  const fresh = (kind: "control" | "param"): ExpressionSetting | undefined => {
    if (kind === "param") {
      const param = params[0];
      return param && { param: param.id, value: param.max };
    }
    const used = new Set(rows.map((r) => r.setting.control));
    const control = controls.find((c) => !used.has(c)) ?? controls[0];
    return control === undefined ? undefined : { control, value: 1 };
  };
  const noun = NOUN[draft.group];
  return (
    <Modal title={`${draft.isNew ? "New" : "Edit"} ${noun} “${draft.name}”`} onClose={onCancel} wide>
      <form
        data-catchlight-expression-editor=""
        onSubmit={(e) => {
          e.preventDefault();
          if (rows.length) onSave(rows.map((r) => r.setting));
        }}
      >
        <p data-catchlight-hint="">
          Each setting gives a control or one of the model’s params a value at full weight.
        </p>
        <div data-catchlight-expression-settings="" role="list" aria-label="Settings">
          {rows.map(({ key, setting }, i) => (
            <SettingRow
              key={key}
              index={i}
              setting={setting}
              controls={controls}
              params={params}
              range={range}
              onKind={(kind) => {
                const replacement = fresh(kind);
                if (replacement) update(key, replacement);
              }}
              onChange={(s) => update(key, s)}
              onRemove={() => setRows((all) => all.filter((r) => r.key !== key))}
            />
          ))}
        </div>
        <button
          type="button"
          data-catchlight-action=""
          disabled={!controls.length && !params.length}
          onClick={() => {
            const setting = fresh(controls.length ? "control" : "param");
            if (setting) setRows((all) => [...all, { key: next.current++, setting }]);
          }}
        >
          <Icon name="plus" />
          Add setting
        </button>
        {!rows.length && (
          <p data-catchlight-hint="" role="status">
            Add at least one setting to save this {noun}.
          </p>
        )}
        <div data-catchlight-dialog-actions="">
          <button type="button" onClick={onCancel}>
            Cancel
          </button>
          <button type="submit" data-primary="" disabled={!rows.length || busy}>
            {busy ? "Saving…" : `Save ${noun}`}
          </button>
        </div>
      </form>
    </Modal>
  );
}

function SettingRow({
  index,
  setting,
  controls,
  params,
  range,
  onKind,
  onChange,
  onRemove,
}: {
  index: number;
  setting: ExpressionSetting;
  controls: string[];
  params: ParamInfo[];
  range: (control: string) => readonly [number, number];
  onKind: (kind: "control" | "param") => void;
  onChange: (setting: ExpressionSetting) => void;
  onRemove: () => void;
}) {
  const n = index + 1;
  const control = setting.control ?? undefined;
  const [min, max] = control !== undefined ? range(control) : [undefined, undefined];
  const missing = setting.param != null && !params.some((p) => p.id === setting.param);
  return (
    <div data-catchlight-expression-setting="" role="listitem">
      <select
        aria-label={`Setting ${n} target`}
        value={control !== undefined ? "control" : "param"}
        onChange={(e) => onKind(e.currentTarget.value as "control" | "param")}
      >
        <option value="control" disabled={!controls.length}>
          Control
        </option>
        <option value="param" disabled={!params.length}>
          Param
        </option>
      </select>
      {control !== undefined ? (
        <select
          aria-label={`Setting ${n} control`}
          value={control}
          onChange={(e) => {
            const to = e.currentTarget.value;
            const [low, high] = range(to);
            onChange({ control: to, value: Math.min(high, Math.max(low, setting.value)) });
          }}
        >
          {controls.map((c) => (
            <option key={c} value={c}>
              {c}
            </option>
          ))}
        </select>
      ) : (
        <select
          aria-label={`Setting ${n} param`}
          value={setting.param ?? ""}
          onChange={(e) => onChange({ param: e.currentTarget.value, value: setting.value })}
        >
          {missing && <option value={setting.param!}>{setting.param} (removed)</option>}
          {params.map((p) => (
            <option key={p.id} value={p.id}>
              {p.name}
            </option>
          ))}
        </select>
      )}
      <NumberField
        label={`Setting ${n} value`}
        value={setting.value}
        {...(min !== undefined ? { min, max } : {})}
        onCommit={(value) =>
          onChange(control !== undefined ? { control, value } : { param: setting.param!, value })
        }
      />
      <IconButton icon="close" label={`Remove setting ${n}`} onClick={onRemove} />
    </div>
  );
}

// React requires a controlled input's `onChange`; the native input event is
// handled once, through `onInput`.
function noop(): void {}
