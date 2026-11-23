/** Follows the active native dialog into the top layer so a failure stays visible. */
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
