// Bridge React 19 ESM into the global namespace so the Ant Design 5 UMD
// bundle (which expects `window.React` / `window.ReactDOM`) sees the same
// React instance as our app.js. Loaded as a module from index.html.

import * as React from "react";
import * as ReactDOM from "react-dom";
import * as ReactDOMClient from "react-dom/client";

window.React = React;
window.ReactDOM = { ...ReactDOM, ...ReactDOMClient };
window.dispatchEvent(new CustomEvent("react-ready"));
