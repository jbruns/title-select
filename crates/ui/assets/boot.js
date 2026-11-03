import init from "/pkg/title-select.js";

// The theme you chose, before the workbench loads, so the page doesn't flash the other one (see parts::theme).
try {
  const theme = localStorage.getItem("title-select.theme");
  if (theme === "light" || theme === "dark") document.documentElement.dataset.theme = theme;
} catch {}

init({ module_or_path: "/pkg/title-select.wasm" });
