/** Every model the editor has open, including other clients' sessions. */

import type { SessionId, SessionInfo } from "@catchlight/core";
import type { ComponentProps } from "react";

import { useSessions } from "./sessions.js";

// `onSelect` is also a DOM event on every element; this one wins.
export interface SessionListRootProps
  extends Omit<ComponentProps<"ul">, "children" | "onSelect"> {
  onSelect?: (info: SessionInfo) => void;
  /** With this set, every row carries a close control. */
  onClose?: (info: SessionInfo) => void;
  current?: SessionId | undefined;
}

export function SessionListRoot({ onSelect, onClose, current, ...rest }: SessionListRootProps) {
  const { sessions } = useSessions();
  return (
    <ul data-catchlight-session-list="" {...rest}>
      {sessions.map((info) => (
        <li
          data-catchlight-session=""
          data-session={info.session}
          data-dirty={info.dirty ? "" : undefined}
          data-current={info.session === current ? "" : undefined}
          key={info.session}
        >
          <button type="button" onClick={() => onSelect?.(info)}>
            {info.title}
          </button>
          {onClose ? (
            <button
              type="button"
              data-catchlight-session-close=""
              aria-label={`Close ${info.title}`}
              onClick={() => onClose(info)}
            >
              {"×"}
            </button>
          ) : null}
        </li>
      ))}
    </ul>
  );
}

export const SessionList = { Root: SessionListRoot };
