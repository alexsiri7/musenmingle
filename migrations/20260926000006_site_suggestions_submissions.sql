-- Public site submissions (POST /v1/suggestions, see src/suggestions.rs).
--
-- Status lifecycle: 'pending' = accepted from the submitter but no GitHub
-- issue filed yet (the ingest run retries); 'accepted' = `new-scraper` issue
-- filed, number stored; 'duplicate' = the domain was already covered or
-- suggested (kept so it counts toward the submitter's rate limit);
-- 'rejected' = set by hand.
--
-- Migrations are APPEND-ONLY: never edit this file once merged.

ALTER TABLE events.site_suggestions
    ADD COLUMN note TEXT NULL CHECK (char_length(note) <= 500);

-- One open suggestion per registrable domain, even under concurrent POSTs.
DROP INDEX events.site_suggestions_domain_idx;
CREATE UNIQUE INDEX site_suggestions_open_domain_key
    ON events.site_suggestions (domain) WHERE status IN ('pending', 'accepted');

CREATE INDEX site_suggestions_ip_created_idx
    ON events.site_suggestions (submitter_ip_hash, created_at);
