/** Opens a picked file as a model. The input is reset so picking the same file again fires again. */

import type { Session } from "@catchlight/core";
import type { ChangeEvent, ComponentProps } from "react";

import { useEditor } from "./editor-context.js";

// `onError` is also a DOM event on every element; this one wins.
export interface FileOpenRootProps
  extends Omit<ComponentProps<"input">, "type" | "onChange" | "onError"> {
  onOpened?: (session: Session) => void;
  onError?: (cause: unknown) => void;
}

export function FileOpenRoot({ onOpened, onError, ...rest }: FileOpenRootProps) {
  const editor = useEditor();

  const handleChange = (event: ChangeEvent<HTMLInputElement>): void => {
    const file = event.currentTarget.files?.[0];
    event.currentTarget.value = "";
    if (!file) return;
    void (async () => {
      try {
        const bytes = new Uint8Array(await file.arrayBuffer());
        onOpened?.(await editor.openFile(bytes, file.name));
      } catch (cause) {
        if (onError) onError(cause);
        else console.warn("catchlight: opening the file failed", cause);
      }
    })();
  };

  return (
    <input
      type="file"
      accept=".clm"
      data-catchlight-file-open=""
      onChange={handleChange}
      {...rest}
    />
  );
}

export const FileOpen = { Root: FileOpenRoot };
