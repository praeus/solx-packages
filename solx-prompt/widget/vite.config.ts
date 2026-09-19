// One level deeper than solx-xprompt's, which sits at its package root - so the
// path to the shared toolkit is `../../` rather than `../`. See the plan's
// "path adjustments this layout forces".
import { createWidgetConfig } from "../../solx-widgets/src/build/widgetViteConfig";

export default createWidgetConfig({
  entry: "src/main.tsx",
  outFile: "solx-prompt.js",
});
