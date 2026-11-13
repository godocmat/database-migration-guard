"""Assert the brief's facts and compare results with the committed observations."""

import argparse
import json
from pathlib import Path

HERE = Path(__file__).resolve().parent
EVIDENCE = HERE / "evidence"
PARSERS = {
    "dmg-pg18": ("pg_query 18.1.0", 180006),
    "dmg-pg17": ("pg_query 6.2.1", 170007),
    "dmg-sql": ("sqlparser 0.64.0 (std)", None),
}


def load(directory, name):
    path = directory / f"{name}.jsonl"
    return [json.loads(line) for line in path.read_bytes().splitlines()]


def fixture(name):
    return (HERE / "fixtures" / f"{name}.sql").read_bytes()


def diagnostics(row, *expected):
    assert [d["code"] for d in row["diagnostics"]] == list(expected), row
    assert row["coverage"] == ("incomplete" if expected else "complete"), row


def facts(row, *kinds):
    actual = [s["facts"] for s in row["statements"]]
    assert [s["kind"] for s in actual] == list(kinds), row
    return actual


def token(file, span, expected):
    start, end = span
    assert file[start:end] == expected, (span, file[start:end], expected)


def cursor(row, byte, line, column):
    expected = {"byte": byte, "line": line, "column": column}
    assert row["diagnostics"][0]["cursor"] == expected, row
    if row.get("batch_error"):
        assert row["batch_error"]["cursor"] == expected, row


def check_matrix(rows, native_version, target):
    quoted = fixture("quoted")
    assert len(quoted) == 166 and quoted.count(b"\r\n") == 3 and not quoted.endswith(b"\n")
    for case, base in (("quoted", 0), ("quoted-segment", 30)):
        row = rows[case]
        assert row["segment_start"] == base and row["parse"] == "accepted"
        diagnostics(row)
        alter, _ = facts(row, "alter_table", "select")
        first, last = row["statements"]
        assert [first["span"], last["span"]] == [[30, 141], [154, 166]]
        token(quoted, first["span"], quoted.split(b"\r\n")[1][:-1])
        token(quoted, last["span"], "SELECT 'é;'".encode())
        token(quoted, first["locations"]["relation_token"], '"café"'.encode())
        assert first["locations"]["relation_token"] == [58, 65]
        assert alter["relation"] == ["App", "café"] and alter["if_exists"]
        add, drop = alter["actions"]
        assert (add["kind"], add["column"], add["if_not_exists"], add["type"]) == ("add_column", "Notes", True, "text")
        assert (drop["kind"], drop["columns"], drop["if_exists"], drop["behavior"]) == ("drop_column", ["Old"], True, "restrict")
        assert drop["anchor"] == {"byte": 105, "line": 2, "column": 75}
        token(quoted, drop["span"], b'DROP COLUMN IF EXISTS "Old" RESTRICT')
        token(quoted, add["span"], b'ADD COLUMN IF NOT EXISTS "Notes" text')

    for case in ("index", "updates", "fk-add", "fk-validate", "after-errors"):
        assert rows[case]["parse"] == "accepted", rows[case]
        diagnostics(rows[case])
    index, = facts(rows["index"], "create_index")
    assert index["relation"] == ["App", "café"] and index["keys"] == ["id"]
    assert index["name"] == "cafe_ix" and index["unique"] and index["concurrent"] and index["if_not_exists"]
    assert index["predicate"] == {"is_not_null": {"column": ["v"]}}
    _, broad, filtered, _ = facts(rows["updates"], "begin", "update", "update", "commit")
    assert broad["predicate"] is None
    assert filtered["predicate"] == {"operator": "=", "left": {"column": ["id"]}, "right": {"integer": 1}}
    for value, update in enumerate((broad, filtered), 1):
        assert update["relation"] == ["t"] and update["assignments"] == [{"column": "v", "value": {"integer": value}}]
    for case, relation, name in (
        ("fk-add", "t", "t_fk"), ("part-fk", "part", "part_fk"), ("part-fk-unknown", "part", "part_fk"),
    ):
        row = rows[case]
        assert row["parse"] == "accepted"
        stmt, = facts(row, "alter_table")
        fk, = stmt["actions"]
        assert stmt["relation"] == [relation]
        assert (fk["kind"], fk["constraint"], fk["name"], fk["columns"]) == ("add_constraint", "foreign_key", name, ["fk"])
        assert (fk["referenced_table"], fk["referenced_columns"], fk["not_valid"]) == (["p"], ["id"], True)
        if native_version:
            assert fk["initially_valid"] is False
    validate, = facts(rows["fk-validate"], "alter_table")
    assert [(a["kind"], a["name"]) for a in validate["actions"]] == [("validate_constraint", "t_fk")]
    diagnostics(rows["part-fk"], *(("target_incompatible",) if target == 17 else ()))
    diagnostics(rows["part-fk-unknown"], *(("target_unresolved",) if target == 17 else ()))

    nn = rows["not-null"]
    if native_version == 180006:
        assert nn["parse"] == "accepted"
        stmt, = facts(nn, "alter_table")
        action, = stmt["actions"]
        assert (action["constraint"], action["name"], action["columns"]) == ("not_null", "t_v_nn", ["v"])
        assert action["not_valid"] is True and action["initially_valid"] is False
        diagnostics(nn, *(("target_incompatible",) if target == 17 else ()))
    else:
        assert nn["parse"] == "rejected"
        diagnostics(nn, "parse")
        cursor(nn, fixture("not-null").index(b"NOT"), 1, 37)
    unique = rows["unique"]
    assert unique["parse"] == ("rejected" if native_version else "accepted")
    diagnostics(unique, "parse" if native_version else "invalid_constraint")
    if native_version == 180006:
        cursor(unique, fixture("unique").index(b"NOT"), 1, 46)

    do = rows["do"]
    assert do["parse"] == ("accepted" if native_version else "rejected")
    diagnostics(do, "unsupported_statement" if native_version else "parse")
    body, select = fixture("do").splitlines()
    token(fixture("do"), do["diagnostics"][0]["span"], body[:-1])
    token(fixture("do"), do["statements"][-1]["span"], select[:-1])
    facts(do, *(("do", "select") if native_version else ("select",)))

    broken, dollar, eof = (rows[name] for name in ("broken", "dollar", "eof"))
    assert broken["parse"] == eof["parse"] == "rejected" and dollar["parse"] == "not_run"
    diagnostics(broken, "parse")
    diagnostics(dollar, "lex")
    diagnostics(eof, "parse")
    cursor(broken, fixture("broken").index(b";", 13), 2, 33)
    token(fixture("broken"), broken["diagnostics"][0]["span"], b"ALTER TABLE t ADD COLUMN broken")
    token(fixture("broken"), broken["statements"][1]["span"], b"DROP TABLE t")
    facts(broken, "select", "drop_table")
    facts(eof, "select")
    assert dollar["diagnostics"][0]["span"] == [0, len(fixture("dollar"))]
    if native_version:
        cursor(dollar, fixture("dollar").index(b"$body$"), 2, 4)
        cursor(eof, len(fixture("eof")), 3, 1)
    else:
        cursor(dollar, len(fixture("dollar")), 3, 1)
        assert "cursor" not in eof["diagnostics"][0]

    for case, code, span in (
        ("directive", "client_directive", [0, 13]),
        ("template", "template", [12, 22]),
        ("nul", "nul", [8, 9]),
        ("invalid-utf8", "invalid_utf8", [8, 9]),
        ("depth", "nesting_limit", [71, 72]),
        ("size", "size_limit", [65536, 65537]),
    ):
        row = rows[case]
        diagnostics(row, code)
        assert row["parse"] == "not_run" and row["diagnostics"][0]["span"] == span, row
    facts(rows["after-errors"], "select")


def check_offline(directory):
    for name, (pin, version) in PARSERS.items():
        rows = load(directory, name)
        assert len(rows) == 42 and all(row["parser"] == pin and row["grammar"] == version for row in rows)
        for target in (17, 18):
            by_case = {row["case"]: row for row in rows if row["target"] == target}
            assert len(by_case) == 21
            check_matrix(by_case, version, target)
        assert rows == load(EVIDENCE, name), f"{name}: recorded output differs"
    print("Offline facts, byte spans, diagnostics, and recorded output match.")


def check_servers(directory):
    for version, number in (
        ("15.20", 150020), ("16.16", 160016), ("17.12", 170012), ("18.7", 180007)
    ):
        rows = load(directory, f"server-{version}")
        assert len(rows) == 12 and all(
            (row["server_version_num"], row["protocol"], row["server_encoding"],
             row["client_encoding"], row["standard_conforming_strings"]) == (number, "simple", "UTF8", "UTF8", "on")
            for row in rows
        )
        by_case = {row["case"]: row for row in rows}
        for case, tags, observation in (
            ("quoted", ["ALTER TABLE", "SELECT 1"], [["id"], ["v"], ["Notes"]]),
            ("index", ["CREATE INDEX"], [["true", "true", "(v IS NOT NULL)"]]),
            ("updates", ["BEGIN", "UPDATE 2", "UPDATE 1", "COMMIT"], [["1", "2"], ["2", "1"]]),
            ("fk-add", ["ALTER TABLE"], [["f", "false"]]),
            ("fk-validate", ["ALTER TABLE"], [["f", "true"]]),
            ("do", ["DO", "SELECT 1"], None),
        ):
            assert [r["tag"] for r in by_case[case]["results"]] == tags
            assert by_case[case]["observation"] == observation
        for case, state, routine, byte in (
            ("unique", "0A000", "processCASbits", 45 if version == "18.7" else None),
            ("broken", "42601", "scanner_yyerror", 45),
            ("dollar", "42601", "scanner_yyerror", 16),
            ("eof", "42601", "scanner_yyerror", len(fixture("eof"))),
        ):
            result, = by_case[case]["results"]
            err = result["error"]
            assert (err["sqlstate"], err["routine"], err.get("byte")) == (state, routine, byte), err
        assert by_case["broken"]["observation"] == [["t"]]
        if version != "18.7":
            assert by_case["part-fk"]["results"][0]["error"]["sqlstate"] == "42809"
            assert by_case["not-null"]["results"][0]["error"]["sqlstate"] == "42601"
        else:
            assert by_case["part-fk"]["observation"] == [["f", "false"]]
            assert by_case["not-null"]["observation"] == [["n", "false"]]
        assert rows == load(EVIDENCE, f"server-{version}"), f"server {version}: recorded output differs"
    print("PostgreSQL 15.20, 16.16, 17.12, and 18.7 simple-query results match.")


if __name__ == "__main__":
    cli = argparse.ArgumentParser(description=__doc__)
    cli.add_argument("--offline", type=Path, help="Directory with dmg-*.jsonl output")
    cli.add_argument("--servers", type=Path, help="Directory with server-*.jsonl output")
    args = cli.parse_args()
    if args.offline:
        check_offline(args.offline)
    if args.servers:
        check_servers(args.servers)
    if not (args.offline or args.servers):
        cli.error("provide --offline, --servers, or both")
