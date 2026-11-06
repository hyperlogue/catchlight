/**
 * Every model the editor has open, kept current.
 *
 * The list is not a replica read: a session another tab or an agent on the
 * socket opened is not in this tab's memory at all, so this is one round trip,
 * repeated whenever the editor says the set changed.
 */

import type { SessionInfo } from "@catchlight/core";
import { useCallback, useEffect, useRef, useState } from "react";

import { useEditor } from "./editor-context.js";

export interface Sessions {
  sessions: SessionInfo[];
  /** Re-asks the editor. Rejects if the editor refuses; the automatic refresh does not. */
  refresh(): Promise<void>;
}

export function useSessions(): Sessions {
  const editor = useEditor();
  const [sessions, setSessions] = useState<SessionInfo[]>([]);

  const request = useRef(0);
  const refresh = useCallback(async (): Promise<void> => {
    const generation = ++request.current;
    const listed = await editor.listSessions();
    // An older response must not restore a model that a newer list removed.
    if (generation === request.current) setSessions(listed);
  }, [editor]);

  useEffect(() => {
    const reload = (): void => {
      void refresh().catch((cause: unknown) => {
        console.warn("catchlight: listing sessions failed", cause);
      });
    };
    reload();
    const off = editor.onSessionsChanged(reload);
    return () => {
      ++request.current;
      off();
    };
  }, [editor, refresh]);

  return { sessions, refresh };
}
