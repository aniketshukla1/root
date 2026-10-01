import React from "react";
import ReactDOM from "react-dom/client";

import "./app.css";

import App from "./App";
import { applyDensity, readDensity } from "./density";
import { applyTheme, readTheme } from "./theme";

// Before anything renders: single sign-on hands the session back in the URL
// fragment, and the app must not decide it is unauthenticated first.
import * as remote from "./remote";
remote.claimTokenFromUrl();

// Both are applied before the first paint so a stored preference never flashes
// the wrong row height or the wrong ground.
applyDensity(readDensity());
applyTheme(readTheme());

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>,
);
