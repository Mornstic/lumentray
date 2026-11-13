import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import Osd from "./Osd";
import Settings from "./Settings";
import "./theme.css";

const view = new URLSearchParams(window.location.search).get("view");

if (view === "settings") {
  document.documentElement.dataset.settings = "wait";
} else if (!view) {
  document.documentElement.dataset.flyout = "wait";
}

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    {view === "settings" ? (
      <Settings />
    ) : view === "osd" ? (
      <Osd />
    ) : (
      <App />
    )}
  </React.StrictMode>,
);
