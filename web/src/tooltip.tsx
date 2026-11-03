import React from "react";
import {
  useFloating,
  offset,
  flip,
  shift,
  autoUpdate,
  Placement,
} from "@floating-ui/react-dom";

interface TooltipProps {
  content: React.ReactNode;
  children: React.ReactElement;
  placement?: Placement;
}

export function Tooltip({ content, children, placement = "top" }: TooltipProps) {
  const [open, setOpen] = React.useState(false);
  const { refs, floatingStyles } = useFloating({
    open,
    placement,
    middleware: [offset(8), flip(), shift({ padding: 8 })],
    whileElementsMounted: autoUpdate,
  });

  const child = React.cloneElement(children, {
    ref: refs.setReference,
    onMouseEnter: () => setOpen(true),
    onMouseLeave: () => setOpen(false),
  } as React.HTMLAttributes<HTMLElement>);

  return (
    <>
      {child}
      {open && content && (
        <div ref={refs.setFloating} style={{ ...floatingStyles, ...TOOLTIP_STYLE }} role="tooltip">
          {content}
        </div>
      )}
    </>
  );
}

const TOOLTIP_STYLE: React.CSSProperties = {
  background: "var(--code-bg, #1e1e1e)",
  color: "var(--fg, #d4d4d4)",
  border: "1px solid var(--border, #444)",
  borderRadius: 6,
  padding: "8px 12px",
  fontSize: "0.8rem",
  fontFamily: "var(--mono, monospace)",
  maxWidth: 380,
  zIndex: 9999,
  boxShadow: "0 4px 12px rgba(0,0,0,0.4)",
  lineHeight: 1.5,
  pointerEvents: "none",
  whiteSpace: "pre-wrap",
  wordBreak: "break-word",
};

export function TipRow({ label, value }: { label: string; value: React.ReactNode }) {
  return (
    <div style={{ display: "flex", gap: 8, justifyContent: "space-between" }}>
      <span style={{ color: "var(--muted, #888)", minWidth: 90 }}>{label}</span>
      <span style={{ fontWeight: 600, textAlign: "right" }}>{value}</span>
    </div>
  );
}

export function TipHeader({ children }: { children: React.ReactNode }) {
  return (
    <div style={{ fontWeight: 700, marginBottom: 5, borderBottom: "1px solid var(--border, #444)", paddingBottom: 4 }}>
      {children}
    </div>
  );
}

export function TipNote({ children, accent }: { children: React.ReactNode; accent?: boolean }) {
  return (
    <div style={{ marginTop: 5, color: accent ? "var(--accent, #4af)" : "var(--muted, #888)", fontSize: "0.75rem" }}>
      {children}
    </div>
  );
}
