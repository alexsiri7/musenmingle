"""Unit tests for the safe-change scope check (run: python3 -m unittest discover .github/safe-change)."""

import json
import os
import unittest

import check

with open(check.POLICY_PATH) as fh:
    POLICY = json.load(fh)


def f(path, status="added", patch=None, **kw):
    d = {"filename": path, "status": status}
    if patch is not None:
        d["patch"] = patch
    d.update(kw)
    return d


def added(*lines):
    return "@@ -0,0 +1,%d @@\n" % len(lines) + "\n".join("+" + l for l in lines)


SCRAPER_RS = added("use crate::sources::jsonld;", "pub const KEY: &str = \"horniman\";", "pub struct Horniman { base: url::Url }")
REGISTRY = (
    "@@ -15,6 +15,7 @@ pub mod barbican;\n pub mod design_museum;\n+pub mod horniman;\n pub mod jsonld;\n"
    "@@ -72,6 +73,7 @@\n         barbican::KEY => Ok(Box::new(barbican::Barbican::new(base))),\n"
    "+        horniman::KEY => Ok(Box::new(horniman::Horniman::new(base))),\n"
)
SEED = added(
    "-- Seed the Horniman scraper, daily.",
    "INSERT INTO events.sources (key, kind, base_url, domain, interval_minutes, enabled,",
    "                            display_name, store_description, store_image, policy_note)",
    "VALUES ('horniman', 'scraper', 'https://www.horniman.ac.uk', 'www.horniman.ac.uk', 1440, TRUE,",
    "        'Horniman Museum', FALSE, FALSE,",
    "        'Terms: facts + link only; don''t copy '",
    "            || '(checked 2026-09-26)')",
    "ON CONFLICT (key) DO NOTHING;",
)


def scraper_pr(*extra):
    return [
        f("src/sources/horniman.rs", patch=SCRAPER_RS),
        f("src/sources/mod.rs", "modified", REGISTRY),
        f("tests/source_horniman.rs", patch=added("#[tokio::test]", "async fn listing() {}")),
        f("tests/fixtures/scrapers/horniman/whats-on.html"),
        f("tests/fixtures/scrapers/horniman/detail/a.html"),
        f("tests/snapshots/source_horniman__horniman_listing.snap"),
        f("migrations/20260926400000_seed_horniman.sql", patch=SEED),
        f("docs/adding-a-scraper.md", "modified", added("more notes")),
        f("README.md", "modified", added("- Horniman Museum")),
        *extra,
    ]


class NewScraper(unittest.TestCase):
    def test_a_typical_scraper_pr_passes(self):
        self.assertEqual(check.evaluate(scraper_pr(), POLICY, "new-scraper"), [])

    def test_ci_deploy_and_build_files_are_never_allowed(self):
        for path in [".github/workflows/ci.yml", "Dockerfile", "Cargo.toml", "Cargo.lock",
                     ".railway/config.json", "ops/x.sh", "build.rs", "src/main.rs"]:
            probs = check.evaluate(scraper_pr(f(path, "modified", added("x"))), POLICY, "new-scraper")
            self.assertTrue(any(path in p for p in probs), path)

    def test_other_app_code_is_outside_the_allowlist(self):
        probs = check.evaluate(scraper_pr(f("src/normalise.rs", "modified", added("fn x() {}"))), POLICY, "new-scraper")
        self.assertEqual(probs, ["src/normalise.rs: outside the new-scraper allowlist"])

    def test_an_existing_source_may_not_be_modified(self):
        probs = check.evaluate(scraper_pr(f("src/sources/barbican.rs", "modified", added("x"))), POLICY, "new-scraper")
        self.assertTrue(any("barbican.rs: modified not allowed" in p for p in probs))

    def test_a_new_source_may_not_reach_process_env_fs_or_unsafe(self):
        for bad in ["use std::process::Command;", "let k = std::env::var(\"X\");", "unsafe { x() }",
                    "const S: &str = include_str!(\"/etc/passwd\");", "let v = env!(\"HOME\");",
                    "std::fs::read_to_string(p)", "sqlx::query(\"DROP\")", "mod helper;"]:
            files = scraper_pr()
            files[0] = f("src/sources/horniman.rs", patch=added("fn ok() {}", bad))
            probs = check.evaluate(files, POLICY, "new-scraper")
            self.assertTrue(any("forbidden code" in p for p in probs), bad)

    def test_registry_changes_are_limited_to_registration_lines(self):
        files = scraper_pr()
        files[1] = f("src/sources/mod.rs", "modified", REGISTRY + "+    std::process::exit(0);\n")
        probs = check.evaluate(files, POLICY, "new-scraper")
        self.assertTrue(any("mod.rs: change outside the allowed lines" in p for p in probs))

    def test_multi_line_registration_is_allowed(self):
        files = scraper_pr()
        files[1] = f("src/sources/mod.rs", "modified", added(
            "pub mod clerkenwell_design_week;",
            "        clerkenwell_design_week::KEY => Ok(Box::new(",
            "            clerkenwell_design_week::ClerkenwellDesignWeek::new(base),",
            "        )),"))
        self.assertEqual(check.evaluate(files, POLICY, "new-scraper"), [])

    def test_renames_and_removals_fail(self):
        self.assertTrue(check.evaluate([f("tests/fixtures/scrapers/x/a.html", "removed")], POLICY, "new-scraper"))
        self.assertTrue(check.evaluate([f("docs/a.md", "renamed", previous_filename=".github/x.yml")], POLICY, "new-scraper"))

    def test_a_file_needing_a_content_check_without_a_diff_fails(self):
        files = scraper_pr()
        files[0] = f("src/sources/horniman.rs")
        self.assertTrue(any("no diff" in p for p in check.evaluate(files, POLICY, "new-scraper")))

    def test_unknown_type_fails(self):
        self.assertEqual(check.evaluate([], POLICY, "venue-request"), ["no safe-change rules for issue type 'venue-request'"])


class Sql(unittest.TestCase):
    T = ["events.sources", "events.refused_sources"]

    def ok(self, sql):
        return check.sql_statements_ok(sql, self.T)[0]

    def test_seed_insert_and_update_pass(self):
        self.assertTrue(self.ok("INSERT INTO events.sources (key, enabled) VALUES ('a', TRUE) ON CONFLICT (key) DO NOTHING;"))
        self.assertTrue(self.ok("insert into events.refused_sources (domain, reason) values ('a.org', 'x' || 'y'), ('b.org', NULL);"))
        self.assertTrue(self.ok("INSERT INTO events.sources (key, n) VALUES ('a', 5) ON CONFLICT (key) DO UPDATE SET n = EXCLUDED.n;"))
        self.assertTrue(self.ok("UPDATE events.sources SET enabled = FALSE, interval_minutes = 1440 WHERE key = 'tate';"))
        self.assertTrue(self.ok("-- comment; with a semicolon\nINSERT INTO events.sources (key) VALUES ('it''s; fine');"))
        self.assertTrue(self.ok("-- the Courtauld's seed\nINSERT INTO events.sources (key, policy_note) VALUES ('c', 'terms say \"as is\" ' || '(checked)');"))

    def test_everything_else_fails(self):
        for sql in [
            "DROP TABLE events.sources;",
            "GRANT ALL ON events.sources TO public;",
            "CREATE TABLE x (a int);",
            "ALTER ROLE musenmingle SUPERUSER;",
            "INSERT INTO events.events (id) VALUES (1);",
            "INSERT INTO events.sources (key) SELECT key FROM events.events;",
            "WITH x AS (DELETE FROM events.events RETURNING 1) INSERT INTO events.sources (key) VALUES ('a');",
            "DO $$ BEGIN PERFORM 1; END $$;",
            "INSERT INTO events.sources (key) VALUES (E'\\x41');",
            "INSERT INTO events.sources (key) VALUES ((SELECT current_user));",
            "UPDATE events.sources SET enabled = TRUE;",
            "UPDATE events.sources SET enabled = TRUE WHERE key = 'a'; DELETE FROM events.events WHERE TRUE;",
            "INSERT INTO \"events\".sources (key) VALUES ('a');",
            "/* x */ INSERT INTO events.sources (key) VALUES ('a');",
            "INSERT INTO events.sources (key) VALUES (pg_read_file('/etc/passwd'));",
            "",
        ]:
            self.assertFalse(self.ok(sql), sql)

    def test_migration_rule_rejects_bad_sql_in_a_pr(self):
        files = scraper_pr()
        files[6] = f("migrations/20260926400000_seed_horniman.sql", patch=added("GRANT ALL ON SCHEMA events TO PUBLIC;"))
        self.assertTrue(any("migrations/" in p for p in check.evaluate(files, POLICY, "new-scraper")))

    def test_an_existing_migration_may_not_be_edited(self):
        probs = check.evaluate([f("migrations/20260101000000_init.sql", "modified", SEED)], POLICY, "new-scraper")
        self.assertTrue(any("modified not allowed" in p for p in probs))


class BugReport(unittest.TestCase):
    def test_app_code_fix_passes_and_config_or_process_does_not(self):
        self.assertEqual(check.evaluate([f("src/normalise.rs", "modified", added("let x = 1;"))], POLICY, "bug-report"), [])
        self.assertTrue(check.evaluate([f("src/config.rs", "modified", added("let x = 1;"))], POLICY, "bug-report"))
        self.assertTrue(check.evaluate([f("src/api.rs", "modified", added("std::process::Command::new(\"sh\")"))], POLICY, "bug-report"))
        self.assertTrue(check.evaluate([f("migrations/x.sql", "added", added("SELECT 1;"))], POLICY, "bug-report"))


class Screened(unittest.TestCase):
    def test_only_screened_without_owner_approval_counts(self):
        nums, kinds = check.screened_kinds([
            {"number": 1, "labels": ["archon:auto-approved", "type:new-scraper"]},
            {"number": 2, "labels": ["archon:auto-approved", "archon:approved", "type:bug-report"]},
            {"number": 3, "labels": ["bug"]},
        ], POLICY)
        self.assertEqual((nums, kinds), ([1], {"new-scraper"}))

    def test_a_screened_issue_without_a_type_fails_closed(self):
        nums, kinds = check.screened_kinds([{"number": 4, "labels": ["archon:auto-approved"]}], POLICY)
        self.assertEqual(kinds, {"<untyped>"})
        self.assertTrue(check.evaluate([], POLICY, "<untyped>"))

    def test_closing_keywords(self):
        self.assertEqual(sorted(int(m) for m in check.CLOSING_RE.findall("Fixes #12, closes #3 and resolved #7; not #9")), [3, 7, 12])


if __name__ == "__main__":
    unittest.main()
