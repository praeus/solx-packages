import { createWidgetConfig } from "../solx-widgets/src/build/widgetViteConfig";

export default createWidgetConfig({
  entry: "src/main.ts",
  outFile: "solx-agent.js",
});
