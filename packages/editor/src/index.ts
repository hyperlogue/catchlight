/**
 * `@catchlight/editor` — the assembled editor, and the one place a default
 * look lives.
 *
 * Layer 4: pure layout over `@catchlight/react`'s parts plus a stylesheet.
 * Everything it draws is a `data-catchlight-*` element the React package
 * already renders, so a host that wants a different arrangement drops this
 * package and keeps the parts.
 *
 * Theming is CSS variables, not props. Every colour, space and font in
 * `theme.css` is a `--cl-*` custom property declared on `.catchlight`, so a
 * host overrides the ones it cares about and inherits the rest. The rules live
 * in `@layer catchlight`, which any unlayered rule in a host's own stylesheet
 * beats without a specificity fight.
 */

export { CatchlightEditor } from "./CatchlightEditor.js";
export type { CatchlightEditorProps } from "./CatchlightEditor.js";

/**
 * The stylesheet, as a host imports it: `import "@catchlight/editor/theme.css"`.
 * Nothing is styled without it.
 */
export const themeStylesheet = "@catchlight/editor/theme.css";
