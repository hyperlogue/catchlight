/**
 * Save, then download the bytes when this tab can read them back. A backend
 * that keeps no readable copy skips the download, and the outcome says so.
 */

import type { Session } from "@catchlight/core";
import { fileKey } from "@catchlight/core";
import { useCallback, useMemo, useRef, useState } from "react";
import type { ComponentProps, FormEvent, ReactNode } from "react";

import { useEditor } from "./editor-context.js";

export interface SaveOutcome {
  key: string;
  downloaded: boolean;
}

export interface FileSaver {
  /** Without `name`, saves where the model was opened from. */
  save(name?: string): Promise<SaveOutcome>;
}

export function useFileSave(session: Session | undefined): FileSaver {
  const editor = useEditor();
  const save = useCallback(
    async (name?: string): Promise<SaveOutcome> => {
      if (!session) throw new Error("Open a model before saving.");
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
  defaultName?: string | undefined;
  onSaved?: (outcome: SaveOutcome) => void;
  onError?: (cause: unknown) => void;
  children?: ReactNode;
}

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
  const [busy, setBusy] = useState(false);

  const handleSubmit = (event: FormEvent<HTMLFormElement>): void => {
    event.preventDefault();
    if (busy) return;
    setBusy(true);
    void save(input.current?.value ?? "")
      .then(
        (outcome) => onSaved?.(outcome),
        (cause: unknown) => {
          if (onError) onError(cause);
          else console.warn("catchlight: saving failed", cause);
        },
      )
      .finally(() => setBusy(false));
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
        disabled={busy}
      />
      <button type="submit" data-catchlight-save-as-submit="" disabled={busy}>
        {busy ? "Saving…" : (children ?? "Save As")}
      </button>
    </form>
  );
}

export const FileSave = { Root: FileSaveRoot };

export function saveKey(name: string): string {
  const key = fileKey(name);
  return /\.clm$/i.test(key) ? key : `${key}.clm`;
}

export function downloadName(key: string): string {
  return key.split("/").pop() || "untitled.clm";
}

// An attached `download` anchor starts a save without a permission prompt;
// some browsers ignore a detached one. The object URL outlives the anchor
// because the download may still be starting.
export function downloadBytes(bytes: Uint8Array, name: string): void {
  const blob = new Blob([bytes as BlobPart], {
    type: "application/octet-stream",
  });
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

const REVOKE_AFTER_MS = 60_000;
