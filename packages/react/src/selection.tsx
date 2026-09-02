/**
 * What is selected, and who else is told.
 *
 * Selection is view state, not document state: it moves no revision and is
 * never undone. It still goes to the editor as presence, because an agent on
 * the socket and another tab both read "what is this person looking at" from
 * there, and a selection only React knows about is invisible to them.
 */

import type { NodeId, Session } from "@catchlight/core";
import { createContext, useCallback, useContext, useMemo, useState } from "react";
import type { ReactNode } from "react";

export interface Selection {
  node: NodeId | undefined;
  select(node: NodeId | undefined): void;
}

const SelectionContext = createContext<Selection | undefined>(undefined);

export interface SelectionProviderProps {
  session: Session;
  children?: ReactNode;
}

export function SelectionProvider({ session, children }: SelectionProviderProps): ReactNode {
  const [node, setNode] = useState<NodeId | undefined>(undefined);

  const select = useCallback(
    (next: NodeId | undefined): void => {
      setNode(next);
      // Fire and forget: presence is a courtesy to other clients, and a
      // selection that failed to publish must not fail the click.
      void session
        .sendPresence({ cmd: "presence_set", pose: [], selection: next ?? null })
        .catch((cause: unknown) => {
          console.warn("catchlight: publishing the selection failed", cause);
        });
    },
    [session],
  );

  const value = useMemo<Selection>(() => ({ node, select }), [node, select]);
  return <SelectionContext.Provider value={value}>{children}</SelectionContext.Provider>;
}

/** The selection of the nearest [`SelectionProvider`]. */
export function useSelection(): Selection {
  const selection = useContext(SelectionContext);
  if (!selection) throw new Error("no <SelectionProvider> above this component");
  return selection;
}
