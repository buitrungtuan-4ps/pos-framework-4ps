import { render } from "solid-js/web";

import { App } from "./App";
import { applyTheme, theme } from "./lib/theme";
import "./app.css";

const root = document.getElementById("root");
if (root === null) {
  throw new Error("the #root element is missing from index.html");
}

// The operator's theme, before the first paint rather than in an effect after it. `App` sets
// `<html lang>` in a `createEffect` and that is fine — a late `lang` is invisible — but a late
// `data-theme` is a flash of the palette the operator did not choose, on every load.
applyTheme(theme(), document.documentElement);

render(() => <App />, root);
