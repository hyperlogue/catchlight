/**
 * `@catchlight/editor-ui`: the assembled editor and its default theme.
 * Theme through the `--cl-*` custom properties on `.catchlight`; rules live in
 * `@layer catchlight`, so any unlayered host rule wins.
 */

export { CatchlightEditor } from "./CatchlightEditor.js";
export type { CatchlightEditorProps } from "./CatchlightEditor.js";

/** Hosts must `import "@catchlight/editor-ui/theme.css"`; nothing is styled without it. */
export const themeStylesheet = "@catchlight/editor-ui/theme.css";
