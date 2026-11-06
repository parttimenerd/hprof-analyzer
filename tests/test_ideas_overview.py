"""Checks for the review page's implementation handoff (no Rust build required)."""

import sys
import importlib.util
import html as html_lib
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "scripts"))

from build_ideas_overview import MAIN, OUT, parse_verdicts  # noqa: E402
from ideas_agent_notes import COMMON, NOTES  # noqa: E402

def priority():
    assert importlib.util.find_spec("ideas_demo_fixtures"), "Priority demo fixtures are missing"
    from ideas_demo_fixtures import PRIORITY
    return PRIORITY


class OverviewHandoffTests(unittest.TestCase):
    def test_priority_demos_cover_every_must_have_and_pool_finalizer_idea(self):
        verdicts = parse_verdicts(MAIN.read_text())
        required = {i for i, v in verdicts.items() if v["rating"].startswith("Must-have")}
        required.update({"P1", "P2", "P3", "P4"})
        fixtures = priority()
        self.assertEqual(set(fixtures), required)
        self.assertGreaterEqual(sum("visual" in fixture for fixture in fixtures.values()), 12)
        for i, fixture in fixtures.items():
            with self.subTest(id=i):
                self.assertTrue(fixture["source"])
                self.assertTrue(fixture["question"])
                self.assertTrue(fixture["takeaway"])
                self.assertTrue(fixture["unavailable"])
                self.assertTrue(fixture["rows"])
                self.assertGreaterEqual(len(fixture["columns"]), 2)
                self.assertTrue(all(len(row) == len(fixture["columns"]) for row in fixture["rows"]))
                if "visual" in fixture:
                    visual = fixture["visual"]
                    self.assertTrue(visual["metric"])
                    self.assertTrue(visual["unit"])
                    self.assertEqual(len(visual["values"]), len(fixture["rows"]))
                    self.assertTrue(all(v >= 0 for v in visual["values"]))

    def test_generated_priority_cards_show_both_example_and_unavailable_state(self):
        html = OUT.read_text()
        for i, fixture in priority().items():
            with self.subTest(id=i):
                card = html.split(f'<article class="card" id="{i}"', 1)[1].split("</article>", 1)[0]
                self.assertIn('class="priority-demo"', card)
                self.assertIn(html_lib.escape(fixture["unavailable"]), card)
                self.assertIn('aria-pressed="true"', card)
                self.assertIn('data-demo-state="unavailable"', card)
                if "visual" in fixture:
                    self.assertIn('class="metric-bars"', card)

    def test_nonpriority_cards_start_collapsed(self):
        html = OUT.read_text()
        self.assertIn('<article class="card collapsed" id="A1"', html)
        self.assertIn('<article class="card" id="A2"', html)

    def test_zero_observed_is_separate_from_analysis_unavailable(self):
        html = OUT.read_text()
        for i in ("E2", "H2", "J5", "P1", "P2"):
            with self.subTest(id=i):
                self.assertIn("zero", priority()[i])
                card = html.split(f'id="{i}"', 1)[1].split('</article>', 1)[0]
                self.assertIn('data-demo-state="zero"', card)

    def test_notes_dont_invent_features_or_enforce_local_cargo_ban(self):
        self.assertNotIn("ThinkStation", " ".join(COMMON))
        self.assertIn("collections.constant_primitive_arrays", NOTES["J4"]["data"])
        self.assertIn("size_distribution", NOTES["C4"]["data"])
        self.assertIn("top.size_distribution.buckets", NOTES["C4"]["data"])
        self.assertIn("upper_bytes; count", NOTES["C4"]["schema"])
        self.assertIn("schema/report.schema.json", " ".join(COMMON))
        self.assertNotIn("update docs/schema.json", " ".join(COMMON))
        self.assertNotIn("reuse charts in web/src/charts.tsx (HBar", " ".join(COMMON))
        self.assertNotIn("Stacked bar of retained", NOTES["B3"]["ui"])
        self.assertNotIn("referent shallow", NOTES["I1"]["ui"])
        self.assertNotIn("d_shallow", NOTES["G3"]["schema"])
        self.assertNotIn("retained × confidence", NOTES["H1"]["ui"])

    def test_detailed_sankey_does_not_claim_retained_bytes_flow(self):
        demos = (ROOT / "docs/visualization-demos.html").read_text()
        section = demos.split('<h2 id="demo-sankey">', 1)[1].split("<h2 ", 1)[0]
        self.assertNotIn("retained bytes flow", section)
        self.assertNotIn("most retained bytes flow", section)

    def test_pool_and_finalizer_demo_do_not_claim_unavailable_metrics(self):
        demos = (ROOT / "docs/visualization-demos.html").read_text()
        pools = demos.split('<h2 id="demo-pools">', 1)[1].split("<h2 ", 1)[0]
        finalizers = demos.split('id="demo-n2"', 1)[1].split('<h2 id="demo-pools">', 1)[0]
        self.assertNotIn("Busy / idle", pools)
        self.assertNotIn("all workers are busy", pools)
        self.assertNotIn("objects waiting for finalization", finalizers)


if __name__ == "__main__":
    unittest.main()