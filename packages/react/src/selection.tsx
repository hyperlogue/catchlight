/**
 * The selection, as the parts that only need that half of presence import it.
 *
 * Selection and pose are published together — see `presence.tsx` for why one
 * command carries both — so the provider is the presence provider under
 * another name. This module keeps the name and the shape the panels were
 * written against.
 */

export { PresenceProvider as SelectionProvider, useSelection } from "./presence.js";
export type { PresenceProviderProps as SelectionProviderProps, Selection } from "./presence.js";
