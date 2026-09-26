import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import "./theme/theme.css";
import "./theme/shell.css";
import "./shell/shell.css";
import { installMocks } from "./api/mocks";
import { App } from "./App";

installMocks();

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <App />
  </StrictMode>,
);
