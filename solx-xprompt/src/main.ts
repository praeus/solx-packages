import { defineReactWidget } from "../../solx-widgets/src/wrap/defineReactWidget";
import { XPromptWidget, type XPromptWidgetFields } from "./XPromptWidget";
import { WIDGET_STYLES } from "./theme";

defineReactWidget<XPromptWidgetFields>("solx-xprompt-widget", XPromptWidget, {
  styles: WIDGET_STYLES,
});
