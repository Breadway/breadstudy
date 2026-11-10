#!/usr/bin/env python3
"""Backfill item rubrics for verified items that lack usable method criteria.

The seeders store worked steps, and the generator writes a method rubric for
every item it inserts. Older rows can still carry `[]`, a bare JSON array, or
an object with an empty method list. This one-off pass:

  * converts a worked-step array into explicit method criteria
  * leaves a valid object rubric alone
  * marks an item with no recoverable working as `answer_only`, so grading
    never awards method marks for a bare final answer

Run inside the image or against a copied database:

  python3 scripts/backfill_rubrics.py --db /var/lib/breadstudy/breadstudy.db
  python3 scripts/backfill_rubrics.py --db ./breadstudy.db --dry-run
"""

import argparse
import json
import os
import sqlite3
import sys

DEFAULT_DB = "/var/lib/breadstudy/breadstudy.db"


def usable_method(value):
    """Return method criteria from an object rubric, or None."""
    if not isinstance(value, dict):
        return None
    method = value.get("method")
    if not isinstance(method, list):
        return None
    criteria = []
    for index, entry in enumerate(method):
        if isinstance(entry, str):
            description = entry.strip()
            marks = 1
            criterion_id = "step-%d" % (index + 1)
        elif isinstance(entry, dict):
            description = str(entry.get("description") or entry.get("criterion") or "").strip()
            marks = entry.get("marks", 1)
            criterion_id = str(entry.get("id") or "step-%d" % (index + 1))
        else:
            continue
        if not description:
            continue
        criteria.append({"id": criterion_id, "description": description, "marks": marks})
    return criteria or None


def worked_criteria(value):
    """Return method criteria from the legacy worked-step string array."""
    if not isinstance(value, list):
        return None
    criteria = []
    for index, entry in enumerate(value):
        if isinstance(entry, str):
            description = entry.strip()
        elif isinstance(entry, dict):
            description = str(entry.get("description") or entry.get("criterion") or "").strip()
        else:
            description = ""
        if description:
            criteria.append(
                {"id": "step-%d" % (index + 1), "description": description, "marks": 1}
            )
    return criteria or None


def normalise(raw, answer):
    """Return the replacement rubric, or None when the row is already usable."""
    try:
        value = json.loads(raw)
    except (TypeError, ValueError):
        value = None

    if isinstance(value, dict) and value.get("answer_only") is True:
        return None
    criteria = usable_method(value)
    if criteria:
        return None
    criteria = worked_criteria(value)
    if criteria:
        return {
            "method": criteria,
            "final": {"marks": 1, "answer": answer},
        }
    return {
        "method": [],
        "final": {"marks": 1, "answer": answer},
        "answer_only": True,
    }


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--db",
        default=os.environ.get("BREADSTUDY_DATABASE_PATH", DEFAULT_DB),
    )
    parser.add_argument("--dry-run", action="store_true")
    args = parser.parse_args()

    connection = sqlite3.connect(args.db, timeout=30)
    connection.row_factory = sqlite3.Row
    rows = connection.execute(
        "SELECT id, answer, rubric FROM items WHERE status = 'verified' ORDER BY id"
    ).fetchall()

    converted = 0
    answer_only = 0
    kept = 0
    for row in rows:
        replacement = normalise(row["rubric"], row["answer"])
        if replacement is None:
            kept += 1
            continue
        if replacement.get("answer_only"):
            answer_only += 1
            action = "answer-only"
        else:
            converted += 1
            action = "method rubric"
        print("%s %s -> %s" % (row["id"], os.path.basename(args.db), action))
        if not args.dry_run:
            connection.execute(
                "UPDATE items SET rubric = ? WHERE id = ?",
                (json.dumps(replacement), row["id"]),
            )

    if not args.dry_run:
        connection.commit()
    print(
        "verified %d: converted %d, answer-only %d, unchanged %d%s"
        % (
            len(rows),
            converted,
            answer_only,
            kept,
            " (dry run)" if args.dry_run else "",
        )
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
