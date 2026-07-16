// React entry for the hprof-analyzer HTML report.
//
// The uncompressed bootstrap (in the HTML shell) has already run: it inflated
// this bundle and injected it, and exposed:
//   - window.__HPROF_DATA_B64__  : base64 of raw-DEFLATE'd report JSON
//   - window.hprofDecodeText(b64): Promise<string> (inflate + UTF-8 decode)
// We decode + inflate + JSON.parse the report data, then render.
import React from "react";
import { createRoot } from "react-dom/client";
import App, { DiffApp } from "./App";
import type { Report, SeriesDiffEnvelope } from "./types";
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
  let parsed: Report | SeriesDiffEnvelope;
  try {
    const json = await decode(b64);
    parsed = JSON.parse(json) as Report | SeriesDiffEnvelope;
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
  // A single-dump Report has no `kind` field; the diff view wraps its payload
  // in a {"kind":"series-diff", diff} envelope so we can dispatch here.
  const isDiff =
    parsed != null &&
    typeof parsed === "object" &&
    (parsed as SeriesDiffEnvelope).kind === "series-diff";
  createRoot(el).render(
    <React.StrictMode>
      {isDiff ? (
        <DiffApp diff={(parsed as SeriesDiffEnvelope).diff} />
      ) : (
        <App report={parsed as Report} />
      )}
    </React.StrictMode>,
  );
}

void boot();
