/**
 * Saving, and getting the bytes out of the tab.
 *
 * A save is a command, and in a tab it lands in the browser's own store — where
 * a reload finds it, and nowhere a person can take it. So a save here is two
 * steps: the command, then the bytes read back and handed to the browser as a
 * download. When the editor keeps no copy this tab can read — it is a process
 * on the machine, writing where it was told — the second step is skipped and
 * the outcome says so, which is a host's cue to show the path instead. Nothing
 * here asks which backend it is on; it asks for the bytes.
 *
 * **The name typed is a storage key.** It is flattened to its last segment and
 * its charset the way every key is (`fileKey`), and given the `.clm` extension
 * when it has none, because that tail is what picks a decoder when the file is
 * opened again.
 */

import type { Session } from "@catchlight/core";
import { fileKey } from "@catchlight/core";
import { useCallback, useMemo, useRef } from "react";
import type { ComponentProps, FormEvent, ReactNode } from "react";

import { useEditor } from "./editor-context.js";

export interface SaveOutcome {
  /** The key the model landed under. */
  key: string;
  /** Whether the bytes were handed to the browser as a download. */
  downloaded: boolean;
}

export interface FileSaver {
  /**
   * Saves under `name`, or where the model was opened from when there is
   * none, and downloads the result when this tab can read it back.
   */
  save(name?: string): Promise<SaveOutcome>;
}

export function useFileSave(session: Session): FileSaver {
  const editor = useEditor();
  const save = useCallback(
    async (name?: string): Promise<SaveOutcome> => {
      const key = await editor.saveSession(session, name === undefined ? undefined : saveKey(name));
      const bytes = await editor.readFile(key);
      if (bytes) downloadBytes(bytes, downloadName(key));
      return { key, downloaded: bytes !== undefined };
    },
    [editor, session],
  );
  return useMemo(() => ({ save }), [save]);
}

// `onError` is also a DOM event on every element; this one wins.
export interface FileSaveRootProps extends Omit<ComponentProps<"form">, "onSubmit" | "onError"> {
  session: Session;
  /** What the name input starts with. */
  defaultName?: string | undefined;
  onSaved?: (outcome: SaveOutcome) => void;
  onError?: (cause: unknown) => void;
  /** The submit control's label. */
  children?: ReactNode;
}

/** A name input and a submit: "Save As". */
export function FileSaveRoot({
  session,
  defaultName,
  onSaved,
  onError,
  children,
  ...rest
}: FileSaveRootProps) {
  const { save } = useFileSave(session);
  const input = useRef<HTMLInputElement>(null);

  const handleSubmit = (event: FormEvent<HTMLFormElement>): void => {
    event.preventDefault();
    void save(input.current?.value ?? "").then(
      (outcome) => onSaved?.(outcome),
      (cause: unknown) => {
        if (onError) onError(cause);
        else console.warn("catchlight: saving failed", cause);
      },
    );
  };

  return (
    <form data-catchlight-file-save="" onSubmit={handleSubmit} {...rest}>
      <input
        type="text"
        data-catchlight-save-as=""
        aria-label="Save as"
        placeholder="name.clm"
        defaultValue={defaultName}
        ref={input}
      />
      <button type="submit" data-catchlight-save-as-submit="">
        {children ?? "Save As"}
      </button>
    </form>
  );
}

export const FileSave = { Root: FileSaveRoot };

/** The key a typed name saves under: flattened, sanitized, and ending in `.clm`. */
export function saveKey(name: string): string {
  const key = fileKey(name);
  return /\.clm$/i.test(key) ? key : `${key}.clm`;
}

/** The file name a download gets: the key's last segment. */
export function downloadName(key: string): string {
  return key.split("/").pop() || "untitled.clm";
}

/**
 * Hands `bytes` to the browser as a file called `name`.
 *
 * An anchor with a `download` attribute is the one way a page starts a save
 * dialog without a permission prompt. It is put in the document for the click
 * — some browsers ignore a detached one — and removed at once; the object URL
 * outlives it, because the download it names may still be starting.
 */
export function downloadBytes(bytes: Uint8Array, name: string): void {
  const blob = new Blob([bytes as BlobPart], { type: "application/octet-stream" });
  const url = URL.createObjectURL(blob);
  const anchor = document.createElement("a");
  anchor.href = url;
  anchor.download = name;
  anchor.setAttribute("data-catchlight-download", "");
  anchor.style.display = "none";
  document.body.append(anchor);
  try {
    anchor.click();
  } finally {
    anchor.remove();
    setTimeout(() => URL.revokeObjectURL(url), REVOKE_AFTER_MS);
  }
}

/** Long enough for a browser to have opened the blob the anchor named. */
const REVOKE_AFTER_MS = 60_000;
