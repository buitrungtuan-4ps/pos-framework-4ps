import { render } from "solid-js/web";

import { App } from "./App";
import { bootTillRules } from "./rules/boot";
import "./app.css";

const root = document.getElementById("root");
if (root === null) {
  throw new Error("the #root element is missing from index.html");
}

render(() => <App />, root);
// The rules module, off the first screen's path (ADR-0193 decision 2).
bootTillRules();
