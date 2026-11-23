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

/** Throws without an `EditorProvider`. */
export function useEditor(): Editor {
  const editor = useContext(EditorContext);
  if (!editor) throw new Error("no <EditorProvider>: every catchlight hook needs an Editor");
  return editor;
}
