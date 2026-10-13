import { defineReactWidget } from "../../solx-widgets/src/wrap/defineReactWidget";
import { InquiryWidget, type InquiryWidgetFields } from "./InquiryWidget";
import { WIDGET_STYLES } from "./theme";

defineReactWidget<InquiryWidgetFields>("solx-inquiry-widget", InquiryWidget, {
  styles: WIDGET_STYLES,
});
