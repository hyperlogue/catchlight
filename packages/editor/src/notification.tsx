/** Keep a failure visible even when a native modal is in the top layer.
 * The notification follows the active dialog, and returns to the workspace
 * when that dialog closes. Its React owner and dismiss action never move. */
import { useEffect, useRef, useState } from "react";
import type { ReactNode } from "react";
import { createPortal } from "react-dom";

export function Notification({ children }: { children: ReactNode }) {
  const anchor = useRef<HTMLSpanElement>(null);
  const [dialog, setDialog] = useState<HTMLDialogElement | null>(null);
  useEffect(() => {
    const root = anchor.current?.closest(".catchlight");
    if (!root) return;
    const refresh = () => setDialog(root.querySelector<HTMLDialogElement>("dialog[open]"));
    const observer = new MutationObserver(refresh);
    observer.observe(root, {
      childList: true,
      subtree: true,
      attributes: true,
      attributeFilter: ["open"],
    });
    refresh();
    return () => observer.disconnect();
  }, []);
  return (
    <>
      <span ref={anchor} hidden />
      {dialog ? createPortal(children, dialog) : children}
    </>
  );
}
