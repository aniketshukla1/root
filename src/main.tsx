import React from "react";
import ReactDOM from "react-dom/client";

import "@fontsource-variable/ibm-plex-sans";
import "@fontsource/ibm-plex-mono/400.css";
import "@fontsource/ibm-plex-mono/500.css";
import "@fontsource/ibm-plex-mono/600.css";
import "./app.css";

import App from "./App";
import { applyDensity, readDensity } from "./density";
import { applyTheme, readTheme } from "./theme";

// Both are applied before the first paint so a stored preference never flashes
// the wrong row height or the wrong ground.
applyDensity(readDensity());
applyTheme(readTheme());

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>,
);
