/** Its own module so the DOM exists before `react-dom` loads. */

import { GlobalRegistrator } from "@happy-dom/global-registrator";

declare global {
  var IS_REACT_ACT_ENVIRONMENT: boolean;
}

if (!globalThis.document) GlobalRegistrator.register();
globalThis.IS_REACT_ACT_ENVIRONMENT = true;
