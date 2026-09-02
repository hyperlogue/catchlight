/**
 * A DOM for one test file.
 *
 * Imported explicitly at the top of every suite here rather than preloaded for
 * the workspace: `@catchlight/core`'s suites prove that package works without a
 * browser, and a global DOM would quietly take that proof away.
 */

import { GlobalRegistrator } from "@happy-dom/global-registrator";

declare global {
  var IS_REACT_ACT_ENVIRONMENT: boolean;
}

if (!globalThis.document) GlobalRegistrator.register();
globalThis.IS_REACT_ACT_ENVIRONMENT = true;
