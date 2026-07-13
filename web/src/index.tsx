// React entry for the hprof-analyzer HTML report.
//
// The uncompressed bootstrap (in the HTML shell) has already run: it inflated
// this bundle and injected it, and exposed:
//   - window.__HPROF_DATA_B64__  : base64 of raw-DEFLATE'd report JSON
//   - window.hprofDecodeText(b64): Promise<string> (inflate + UTF-8 decode)
// We decode + inflate + JSON.parse the report data, then render.
import React from "react";
import { createRoot } from "react-dom/client";
import App from "./App";
import type { Report } from "./types";
import css from "./styles.css";

function fail(msg: string): void {
  const root = document.getElementById("root");
  if (root) root.textContent = msg;
}

function injectStyles(): void {
  const style = document.createElement("style");
  style.textContent = css as unknown as string;
  document.head.appendChild(style);
}

async function boot(): Promise<void> {
  const b64 = window.__HPROF_DATA_B64__ || "";
  const decode = window.hprofDecodeText;
  if (!decode) {
    fail("Report bootstrap missing (hprofDecodeText).");
    return;
  }
  let report: Report;
  try {
    const json = await decode(b64);
    report = JSON.parse(json) as Report;
  } catch (e) {
    fail("Failed to parse report data: " + e);
    return;
  }
  injectStyles();
  const el = document.getElementById("root");
  if (!el) {
    fail("Missing #root element.");
    return;
  }
  el.textContent = "";
  createRoot(el).render(
    <React.StrictMode>
      <App report={report} />
    </React.StrictMode>,
  );
}

void boot();
