// Imported per suite, not preloaded: `@catchlight/core`'s suites must run without a DOM.

import { GlobalRegistrator } from "@happy-dom/global-registrator";

declare global {
  var IS_REACT_ACT_ENVIRONMENT: boolean;
}

if (!globalThis.document) GlobalRegistrator.register();
globalThis.IS_REACT_ACT_ENVIRONMENT = true;
