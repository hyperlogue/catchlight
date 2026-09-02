/**
 * A DOM for this package's one suite.
 *
 * Imported first, and from its own module, because a registrator called in the
 * suite's own body would run after `react-dom` had already loaded against no
 * document.
 */

import { GlobalRegistrator } from "@happy-dom/global-registrator";

declare global {
  var IS_REACT_ACT_ENVIRONMENT: boolean;
}

if (!globalThis.document) GlobalRegistrator.register();
globalThis.IS_REACT_ACT_ENVIRONMENT = true;
