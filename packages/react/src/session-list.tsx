/**
 * Every document the editor has open, including the ones this tab did not
 * open.
 *
 * That is the whole point of showing it: an agent driving the same editor over
 * the socket has sessions of its own, and this is where a person finds them —
 * and closes them, since a close is a command on the editor and not on any
 * replica this tab holds.
 */

import type { SessionId, SessionInfo } from "@catchlight/core";
import type { ComponentProps } from "react";

import { useSessions } from "./sessions.js";

// `onSelect` is also a DOM event on every element; this one wins.
export interface SessionListRootProps
  extends Omit<ComponentProps<"ul">, "children" | "onSelect"> {
  onSelect?: (info: SessionInfo) => void;
  /** With this set, every row carries a close control. */
  onClose?: (info: SessionInfo) => void;
  /** The session a host is showing, marked `data-current` on its row. */
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
