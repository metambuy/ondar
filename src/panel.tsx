// Entry for panel.html. Mounts the popover's React tree and nothing else.
import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import "./styles/tokens.css";
import Panel from "./panel/Panel";

const root = document.getElementById("root");
if (root) {
  createRoot(root).render(
    <StrictMode>
      <Panel />
    </StrictMode>,
  );
}
