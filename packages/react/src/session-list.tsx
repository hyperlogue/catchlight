/**
 * Every document the editor has open, including the ones this tab did not
 * open.
 *
 * That is the whole point of showing it: an agent driving the same editor over
 * the socket has sessions of its own, and this is where a person finds them.
 */

import type { SessionInfo } from "@catchlight/core";
import type { ComponentProps } from "react";

import { useSessions } from "./sessions.js";

// `onSelect` is also a DOM event on every element; this one wins.
export interface SessionListRootProps
  extends Omit<ComponentProps<"ul">, "children" | "onSelect"> {
  onSelect?: (info: SessionInfo) => void;
}

export function SessionListRoot({ onSelect, ...rest }: SessionListRootProps) {
  const { sessions } = useSessions();
  return (
    <ul data-catchlight-session-list="" {...rest}>
      {sessions.map((info) => (
        <li
          data-catchlight-session=""
          data-session={info.session}
          data-dirty={info.dirty ? "" : undefined}
          key={info.session}
        >
          <button type="button" onClick={() => onSelect?.(info)}>
            {info.title}
          </button>
        </li>
      ))}
    </ul>
  );
}

export const SessionList = { Root: SessionListRoot };
