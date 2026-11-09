"""STUDY-11 checks: due skills, SymPy gate, dedupe, sources, audio."""
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from pipeline import TTSClient, build_items, run_nightly, sympy_check


def main() -> int:
    failures = []
    import tempfile
    with tempfile.TemporaryDirectory() as td:
        tmp = Path(td)
        audio = tmp / "audio"

        def fake_source(skill_id):
            return ("https://example.test/" + skill_id,
                    "worked example for " + skill_id, "2026-11-09T00:00:00+00:00")

        def fake_synth(text, dest):
            dest.write_bytes(b"RIFF-fake-wav:" + text.encode()[:16])
            return True

        # SymPy gate holds over every generated item.
        for skill in ("volume-prism-cylinder", "volume-cone-pyramid-sphere",
                      "surface-area", "unit-conversion-scaling"):
            for item in build_items(skill, "2026-11-09"):
                if not sympy_check(item.answer_expr, item.answer_value):
                    failures.append("SymPy failed: " + item.id)
        if sympy_check("pi*3**2*7", 1.0):
            failures.append("SymPy gate accepts a wrong value")

        # One run picks due skills and generates 2-3 items per skill.
        due = ["volume-prism-cylinder", "surface-area"]
        tts = TTSClient(out_dir=audio, synth=fake_synth, backend="fake-test")
        result = run_nightly(due, [], "2026-11-09", audio, tts=tts,
                             source_fetcher=fake_source)
        per_skill: dict[str, int] = {}
        for entry in result.items:
            per_skill[entry["skill_id"]] = per_skill.get(entry["skill_id"], 0) + 1
        for skill in due:
            count = per_skill.get(skill, 0)
            if not 2 <= count <= 3:
                failures.append("wrong count for %s: %d" % (skill, count))
        if result.failed_verify:
            failures.append("verify failures in clean run")
        for entry in result.items:
            if not entry["source_url"] or not entry["source_snippet"] or not entry["fetched_at"]:
                failures.append("missing source fields: " + entry["id"])
            if len(entry["audio"]) != 1 + len(entry["hints"]):
                failures.append("audio missing for prompt or hints: " + entry["id"])
            for part in entry["audio"]:
                if part["status"] != "rendered":
                    failures.append("audio not rendered: " + entry["id"])
                if not (audio / part["file"]).exists():
                    failures.append("audio file missing: " + part["file"])

        # Dedupe: a second identical run adds nothing.
        again = run_nightly(due, result.items, "2026-11-09", audio, tts=tts,
                            source_fetcher=fake_source)
        if again.items:
            failures.append("dedupe failed: second run added %d" % len(again.items))
        if again.skipped_dupes != len(result.items):
            failures.append("dedupe count wrong: %d" % again.skipped_dupes)

    if failures:
        for f in failures:
            print("FAIL: " + f)
        return 1
    print("PASS")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
