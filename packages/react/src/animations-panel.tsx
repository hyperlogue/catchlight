import type { ClipInfo, Session } from "@catchlight/core";

import type { ErrorHandler } from "./authoring.js";
import { EmptyState, Icon } from "./controls.js";
import {
  ANIMATIONS,
  ExtensionWarnings,
  UnreadNotice,
  WarningMark,
  plural,
  useClips,
  useExtensionWarnings,
  usePlayhead,
  usePlayingClip,
  warningsOn,
} from "./official.js";

/** What a clip's lanes drive, by kind, as in "3 params, 2 controls, 1 viseme". */
export function describeLanes(clip: ClipInfo): string {
  const count = { param: 0, control: 0, expression: 0, viseme: 0 };
  for (const lane of clip.lanes) count[lane.target.kind] += 1;
  const parts = [
    count.param && plural(count.param, "param"),
    count.control && plural(count.control, "control"),
    count.expression && plural(count.expression, "expression"),
    count.viseme && plural(count.viseme, "viseme"),
  ].filter(Boolean);
  return parts.length ? parts.join(", ") : "No lanes";
}

export function clipSeconds(clip: ClipInfo): number {
  return clip.length * clip.timestep;
}

/** The model's animations, each played in this tab only. */
export function AnimationsPanel({ session, onError }: { session: Session; onError: ErrorHandler }) {
  const read = useClips(session);
  const { warnings, params: warned } = useExtensionWarnings(session, ANIMATIONS);
  const playing = usePlayingClip(session);
  if (read.unread !== undefined)
    return (
      <div data-catchlight-animations-panel="">
        <UnreadNotice what="animations" why={read.unread} />
      </div>
    );
  const { clips } = read.body;
  return (
    <div data-catchlight-animations-panel="">
      <ExtensionWarnings warnings={warnings} />
      {!clips.length ? (
        <EmptyState icon="film" title="No animations">
          Animations come from an import or the command line. Once the model has one, play it here.
        </EmptyState>
      ) : (
        <ul data-catchlight-clips="" aria-label="Animations">
          {clips.map((clip) => {
            const on = playing === clip.name;
            const marks = warningsOn(
              warned,
              clip.lanes.flatMap((l) => (l.target.kind === "param" ? [l.target.param] : [])),
            );
            return (
              <li
                key={clip.name}
                data-catchlight-clip=""
                data-name={clip.name}
                data-playing={on ? "" : undefined}
                data-warned={marks.length ? "" : undefined}
              >
                <button
                  type="button"
                  data-catchlight-clip-play=""
                  aria-pressed={on}
                  aria-label={on ? `Stop ${clip.name}` : `Play ${clip.name}`}
                  title={on ? "Stop" : "Play"}
                  onClick={() => {
                    if (on) session.stop();
                    else if (!session.play(clip.name))
                      onError(new Error(`The animation “${clip.name}” could not be played.`));
                  }}
                >
                  <Icon name={on ? "stop" : "play"} width="13" height="13" />
                </button>
                <span data-catchlight-clip-name="" title={clip.name}>
                  {clip.name}
                </span>
                <WarningMark warnings={marks} />
                <span data-catchlight-clip-lanes="">{describeLanes(clip)}</span>
                {on ? (
                  <Playhead session={session} clip={clip} />
                ) : (
                  <span data-catchlight-clip-duration="">{seconds(clipSeconds(clip))}</span>
                )}
              </li>
            );
          })}
        </ul>
      )}
    </div>
  );
}

/** Re-renders every frame while the clip plays, apart from the list around it. */
function Playhead({ session, clip }: { session: Session; clip: ClipInfo }) {
  const at = usePlayhead(session);
  if (!at || at.name !== clip.name) return null;
  return (
    <span data-catchlight-clip-playhead="" aria-label={`${clip.name} playhead`}>
      {seconds(at.time)} / {seconds(clipSeconds(clip))}
      <small>frame {Math.floor(at.frame)}</small>
    </span>
  );
}

function seconds(s: number): string {
  return `${s.toFixed(2)} s`;
}
