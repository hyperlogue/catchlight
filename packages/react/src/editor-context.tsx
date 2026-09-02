/**
 * Where the editor lives for everything below it.
 *
 * One editor per tree: it owns the GPU device, and every session, viewport and
 * panel under this provider shares it. Passing it through context rather than
 * a module singleton is what lets a test mount two editors side by side, and
 * what keeps the package from ever constructing one itself — acquiring a
 * device is the host's call, and it is asynchronous.
 */

import type { Editor } from "@catchlight/core";
import { createContext, useContext } from "react";
import type { ReactNode } from "react";

const EditorContext = createContext<Editor | undefined>(undefined);

export interface EditorProviderProps {
  editor: Editor;
  children?: ReactNode;
}

export function EditorProvider({ editor, children }: EditorProviderProps): ReactNode {
  return <EditorContext.Provider value={editor}>{children}</EditorContext.Provider>;
}

/**
 * The editor this tree runs against.
 *
 * Throws rather than returning `undefined`: every hook and part in this
 * package needs one, so a missing provider is a wiring mistake to report at
 * the point it happens, not a state to branch on.
 */
export function useEditor(): Editor {
  const editor = useContext(EditorContext);
  if (!editor) throw new Error("no <EditorProvider>: every catchlight hook needs an Editor");
  return editor;
}
