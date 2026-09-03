import { defineReactWidget } from "../../solx-widgets/src/wrap/defineReactWidget";
import { AgentWidget, type AgentWidgetFields } from "./AgentWidget";
import { WIDGET_STYLES } from "./theme";

defineReactWidget<AgentWidgetFields>("solx-agent-widget", AgentWidget, {
  styles: WIDGET_STYLES,
});
