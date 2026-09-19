import { createWidgetConfig } from "../solx-widgets/src/build/widgetViteConfig";

export default createWidgetConfig({
  entry: "src/main.tsx",
  outFile: "solx-xprompt.js",
});
