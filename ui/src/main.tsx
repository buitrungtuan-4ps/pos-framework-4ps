import { render } from "solid-js/web";

import { App } from "./App";
import "./app.css";

const root = document.getElementById("root");
if (root === null) {
  throw new Error("the #root element is missing from index.html");
}

render(() => <App />, root);
