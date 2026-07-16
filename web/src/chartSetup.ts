// Tree-shaken Chart.js registration. Import this module once (side-effecting)
// before any react-chartjs-2 <Pie>/<Bar> renders. We register ONLY the
// controllers, elements, scales, and plugins the report's charts use, so the
// bundle stays small.
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
