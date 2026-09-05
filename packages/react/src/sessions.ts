/**
 * Every model the editor has open, kept current.
 *
 * The list is not a replica read: a session another tab or an agent on the
 * socket opened is not in this tab's memory at all, so this is one round trip,
 * repeated whenever the editor says the set changed.
 */

import type { SessionInfo } from "@catchlight/core";
import { useCallback, useEffect, useState } from "react";

import { useEditor } from "./editor-context.js";

export interface Sessions {
  sessions: SessionInfo[];
  /** Re-asks the editor. Rejects if the editor refuses; the automatic refresh does not. */
  refresh(): Promise<void>;
}

export function useSessions(): Sessions {
  const editor = useEditor();
  const [sessions, setSessions] = useState<SessionInfo[]>([]);

  const refresh = useCallback(async (): Promise<void> => {
    setSessions(await editor.listSessions());
  }, [editor]);

  useEffect(() => {
    let live = true;
    const reload = (): void => {
      void editor
        .listSessions()
        .then((listed) => {
          if (live) setSessions(listed);
        })
        .catch((cause: unknown) => {
          // Nobody asked for this one, so there is nowhere to reject to.
          console.warn("catchlight: listing sessions failed", cause);
        });
    };
    reload();
    const off = editor.onSessionsChanged(reload);
    return () => {
      live = false;
      off();
    };
  }, [editor]);

  return { sessions, refresh };
}
