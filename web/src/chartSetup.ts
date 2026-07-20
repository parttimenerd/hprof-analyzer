// Tree-shaken Chart.js registration. Import this module once (side-effecting)
// before any react-chartjs-2 <Pie>/<Bar> renders. We register ONLY the
// controllers, elements, scales, and plugins the report's charts use, so the
// bundle stays small.
import React from "react";
import {
  Chart as ChartJS,
  ArcElement,
  BarElement,
  CategoryScale,
  LinearScale,
  Tooltip,
  Legend,
} from "chart.js";

ChartJS.register(ArcElement, BarElement, CategoryScale, LinearScale, Tooltip, Legend);

// Chart.js draws to <canvas> and cannot read CSS custom properties, so pull the
// current theme colors from the document root at render time. Recomputed on each
// call so a runtime theme toggle picks up new values on the next chart mount.
export function themeColors(): { fg: string; muted: string; border: string; bg: string } {
  const cs = getComputedStyle(document.documentElement);
  const v = (name: string, fallback: string) => {
    const raw = cs.getPropertyValue(name).trim();
    return raw.length ? raw : fallback;
  };
  return {
    fg: v("--fg", "#1a1a1a"),
    muted: v("--muted", "#666"),
    border: v("--border", "#e2e2e2"),
    bg: v("--bg", "#ffffff"),
  };
}

// Sync Chart.js global defaults to the current theme so legend labels, tick
// labels, and grid lines all inherit the right color without needing per-chart
// overrides. Called once on load and again whenever data-theme changes.
function syncChartDefaults() {
  const t = themeColors();
  ChartJS.defaults.color = t.fg;
  ChartJS.defaults.borderColor = t.border;
}

syncChartDefaults();
new MutationObserver(syncChartDefaults).observe(document.documentElement, {
  attributes: true,
  attributeFilter: ["data-theme"],
});

// Returns the current data-theme attribute value ("light" | "dark" | "").
// Charts should use this as a `key` prop so they remount — and re-read
// themeColors() — whenever the user toggles the theme.
export function useThemeKey(): string {
  const [theme, setTheme] = React.useState(
    () => document.documentElement.getAttribute("data-theme") ?? ""
  );
  React.useEffect(() => {
    const obs = new MutationObserver(() => {
      setTheme(document.documentElement.getAttribute("data-theme") ?? "");
    });
    obs.observe(document.documentElement, { attributes: true, attributeFilter: ["data-theme"] });
    return () => obs.disconnect();
  }, []);
  return theme;
}
