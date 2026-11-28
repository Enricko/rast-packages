// Code Editor as a Package Center app: the window module the package ships
// (ui/app.js + ui/app.css, built by scripts/build-ui.sh).
import type { AppContext } from "../../core/app";
import "./code.css";
import { mountCodeEditor } from "./index";

export default {
  mount: (ctx: AppContext) => mountCodeEditor(ctx),
};
