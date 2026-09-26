#!/usr/bin/env python3
"""safe-change: scope check for PRs built from screened public submissions.

The factory (interstellarai.net ops/cron) screens issues that public
channels file (site suggestions, Sentry events, ...) and labels the ones it
lets an agent work `archon:auto-approved` plus `type:<kind>`. A PR that
closes such an issue may only touch what that kind of change needs; this
check fails it otherwise, and the factory's pr-maintenance does not merge a
PR whose check is not green.

Runs as `pull_request_target`, so this file and the policy
(.github/safe-change.json) come from the base branch: a PR cannot weaken the
check that judges it. It never checks out or runs PR code; it reads the PR's
file list and patches from the API as data.

PRs that close no screened-only issue pass at once ("not applicable").

Usage (CI): GITHUB_TOKEN=... GITHUB_REPOSITORY=owner/repo PR_NUMBER=n check.py
"""

from __future__ import annotations

import fnmatch
import json
import os
import re
import sys
import urllib.request

HERE = os.path.dirname(os.path.abspath(__file__))
POLICY_PATH = os.path.join(HERE, "..", "safe-change.json")

CLOSING_RE = re.compile(r"\b(?:close[sd]?|fix(?:e[sd])?|resolve[sd]?)\s+#(\d+)\b", re.I)


# ── policy evaluation (pure; unit-tested) ────────────────────────────────────


def _match(path: str, rule: dict) -> bool:
    if "path" in rule:
        return path == rule["path"]
    return fnmatch.fnmatchcase(path, rule["glob"])


def _patch_lines(patch: str | None) -> tuple[list[str], list[str]]:
    """Added and removed lines of a unified diff hunk list."""
    added, removed = [], []
    for line in (patch or "").splitlines():
        if line.startswith("+++") or line.startswith("---"):
            continue
        if line.startswith("+"):
            added.append(line[1:])
        elif line.startswith("-"):
            removed.append(line[1:])
    return added, removed


def _strip_sql(sql: str) -> str | None:
    """SQL with string literals replaced by '?' and comments removed; None if
    it uses a quoting form we do not parse (dollar quotes, E'' strings,
    quoted identifiers, block comments)."""
    out, i, n = [], 0, len(sql)
    while i < n:
        c = sql[i]
        if c in '$"' or sql.startswith("/*", i):
            return None  # only checked outside literals and comments
        if c == "-" and sql.startswith("--", i):
            j = sql.find("\n", i)
            i = n if j < 0 else j
            continue
        if c == "'":
            if i > 0 and sql[i - 1] in "eEuUbBxX":
                return None
            j = i + 1
            while True:
                k = sql.find("'", j)
                if k < 0:
                    return None  # unterminated
                if sql.startswith("''", k):
                    j = k + 2
                    continue
                break
            out.append("?")
            i = k + 1
            continue
        out.append(c)
        i += 1
    return "".join(out)


_IDENT = r"[a-z_][a-z0-9_]*"
_VALUE = r"(?:\?|-?\d+(?:\.\d+)?|true|false|null|excluded\.%s)(?:\s*::\s*%s)?" % (_IDENT, _IDENT)
_EXPR = r"%s(?:\s*\|\|\s*%s)*" % (_VALUE, _VALUE)
_ROW = r"\(\s*%s(?:\s*,\s*%s)*\s*\)" % (_EXPR, _EXPR)
_COLS = r"\(\s*%s(?:\s*,\s*%s)*\s*\)" % (_IDENT, _IDENT)
_ASSIGN = r"%s\s*=\s*%s" % (_IDENT, _EXPR)
_CONFLICT = r"on\s+conflict\s*%s\s*do\s+(?:nothing|update\s+set\s+%s(?:\s*,\s*%s)*)" % (
    _COLS, _ASSIGN, _ASSIGN)


def sql_statements_ok(sql: str, tables: list[str]) -> tuple[bool, str]:
    """Only INSERT into / UPDATE of the allowed tables, literal values only:
    no DDL, no GRANT, no SELECT/WITH/DO, no other tables."""
    stripped = _strip_sql(sql)
    if stripped is None:
        return False, "unsupported SQL quoting (dollar quotes, E'' strings, quoted identifiers or block comments)"
    tabs = "|".join(re.escape(t.lower()) for t in tables)
    insert = re.compile(r"insert\s+into\s+(?:%s)\s*%s\s*values\s*%s(?:\s*,\s*%s)*(?:\s*%s)?" % (
        tabs, _COLS, _ROW, _ROW, _CONFLICT))
    update = re.compile(r"update\s+(?:%s)\s+set\s+%s(?:\s*,\s*%s)*\s+where\s+%s(?:\s+and\s+%s)*" % (
        tabs, _ASSIGN, _ASSIGN, _ASSIGN, _ASSIGN))
    stmts = [s.strip() for s in stripped.lower().split(";") if s.strip()]
    if not stmts:
        return False, "no statements"
    for s in stmts:
        s = re.sub(r"\s+", " ", s)
        if not (insert.fullmatch(s) or update.fullmatch(s)):
            return False, "statement is not a literal INSERT/UPDATE of %s: %.80s" % (", ".join(tables), s)
    return True, ""


def evaluate(files: list[dict], policy: dict, kind: str) -> list[str]:
    """Problems with the PR's files for issue kind `kind` (empty = pass).

    Each file: {filename, status, patch?, previous_filename?} as the GitHub
    pulls/files API returns them.
    """
    problems: list[str] = []
    rules = policy.get("types", {}).get(kind)
    if rules is None:
        return ["no safe-change rules for issue type %r" % kind]
    deny = policy.get("deny", [])
    for f in files:
        path, status = f["filename"], f["status"]
        paths = [path] + ([f["previous_filename"]] if f.get("previous_filename") else [])
        if any(fnmatch.fnmatchcase(p, g) for p in paths for g in deny):
            problems.append("%s: never allowed in a screened change" % path)
            continue
        if status == "renamed":
            problems.append("%s: renames are not allowed" % path)
            continue
        # An exact "path" rule wins over globs; otherwise the first glob.
        matching = [r for r in rules if _match(path, r)]
        rule = next((r for r in matching if "path" in r), matching[0] if matching else None)
        if rule is None:
            problems.append("%s: outside the %s allowlist" % (path, kind))
            continue
        if status not in rule.get("status", ["added", "modified"]):
            problems.append("%s: %s not allowed here (only %s)" % (path, status, "/".join(rule.get("status", []))))
            continue
        added, removed = _patch_lines(f.get("patch"))
        needs_patch = rule.get("content_deny") or rule.get("line_patterns") or rule.get("sql_tables")
        if needs_patch and f.get("patch") is None:
            problems.append("%s: no diff available to check" % path)
            continue
        for pat in rule.get("content_deny", []):
            if any(re.search(pat, line) for line in added):
                problems.append("%s: adds forbidden code (%s)" % (path, pat))
        if rule.get("line_patterns"):
            pats = [re.compile(p) for p in rule["line_patterns"]]
            for line in added + removed:
                if line.strip() and not any(p.fullmatch(line) for p in pats):
                    problems.append("%s: change outside the allowed lines: %.80s" % (path, line.strip()))
                    break
        if rule.get("sql_tables"):
            ok, why = sql_statements_ok("\n".join(added), rule["sql_tables"])
            if not ok:
                problems.append("%s: %s" % (path, why))
    return problems


def screened_kinds(issues: list[dict], policy: dict) -> tuple[list[int], set[str]]:
    """Issues only automated screening vetted, and their type:<kind> labels."""
    screened_label = policy.get("screened_label", "archon:auto-approved")
    approved_label = policy.get("approved_label", "archon:approved")
    nums, kinds = [], set()
    for issue in issues:
        labels = set(issue["labels"])
        if screened_label in labels and approved_label not in labels:
            nums.append(issue["number"])
            kinds |= {l[len("type:"):] for l in labels if l.startswith("type:")} or {"<untyped>"}
    return nums, kinds


# ── GitHub I/O ───────────────────────────────────────────────────────────────


def _api(url: str, token: str, data: bytes | None = None):
    req = urllib.request.Request(url, data=data, headers={
        "Authorization": "Bearer " + token,
        "Accept": "application/vnd.github+json",
        "User-Agent": "safe-change-check",
    })
    with urllib.request.urlopen(req, timeout=30) as r:
        return json.load(r)


def main() -> int:
    token = os.environ["GITHUB_TOKEN"]
    repo = os.environ["GITHUB_REPOSITORY"]
    pr = int(os.environ["PR_NUMBER"])
    owner, name = repo.split("/", 1)
    with open(POLICY_PATH) as fh:
        policy = json.load(fh)

    base = "https://api.github.com/repos/%s" % repo
    pull = _api("%s/pulls/%d" % (base, pr), token)
    q = {"query": "query($o:String!,$n:String!,$p:Int!){repository(owner:$o,name:$n){pullRequest(number:$p)"
                  "{closingIssuesReferences(first:20){nodes{number}}}}}",
         "variables": {"o": owner, "n": name, "p": pr}}
    gql = _api("https://api.github.com/graphql", token, json.dumps(q).encode())
    numbers = {n["number"] for n in gql["data"]["repository"]["pullRequest"]["closingIssuesReferences"]["nodes"]}
    numbers |= {int(m) for m in CLOSING_RE.findall(pull.get("body") or "")}
    issues = []
    for n in sorted(numbers):
        issue = _api("%s/issues/%d" % (base, n), token)
        issues.append({"number": n, "labels": [l["name"] for l in issue.get("labels", [])]})

    nums, kinds = screened_kinds(issues, policy)
    if not nums:
        print("safe-change: not applicable (the PR closes no issue that only automated screening vetted)")
        return 0

    files, page = [], 1
    while True:
        batch = _api("%s/pulls/%d/files?per_page=100&page=%d" % (base, pr, page), token)
        files += batch
        if len(batch) < 100:
            break
        page += 1

    problems = []
    for kind in sorted(kinds):
        problems += evaluate(files, policy, kind)
    closes = ", ".join("#%d" % n for n in nums)
    if problems:
        print("safe-change: FAILED for %s (type %s):" % (closes, "/".join(sorted(kinds))))
        for p in problems:
            print("  - " + p)
        return 1
    print("safe-change: ok — %d file(s) within the %s allowlist (closes %s)" % (
        len(files), "/".join(sorted(kinds)), closes))
    return 0


if __name__ == "__main__":
    sys.exit(main())
