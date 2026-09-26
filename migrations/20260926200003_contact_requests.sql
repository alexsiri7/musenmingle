-- Requests from venues and site owners sent with the /contact form
-- (src/contact.rs). Each becomes (or is added as a comment to) a
-- `venue-request` GitHub issue that references only the row id: the optional
-- reply email is kept here and NEVER posted to GitHub.
--
-- status: pending_issue (not on GitHub yet; the next ingest run files it),
--         filed (created github_issue_number),
--         commented (added to an earlier request's issue: same domain and
--                    type within 7 days).
--
-- Migrations are APPEND-ONLY: never edit this file once merged.

CREATE TABLE events.contact_requests (
    id                  BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    request_type        TEXT        NOT NULL
        CHECK (request_type IN ('remove_listings', 'correct_event', 'other')),
    url                 TEXT        NOT NULL CHECK (char_length(url) <= 2048),
    domain              TEXT        NOT NULL,
    details             TEXT        NOT NULL CHECK (char_length(details) <= 2000),
    reply_email         TEXT        NULL CHECK (char_length(reply_email) <= 254),
    ip_hash             TEXT        NOT NULL,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    github_issue_number BIGINT      NULL,
    status              TEXT        NOT NULL DEFAULT 'pending_issue'
        CHECK (status IN ('pending_issue', 'filed', 'commented'))
);

CREATE INDEX contact_requests_ip_created_idx ON events.contact_requests (ip_hash, created_at);
CREATE INDEX contact_requests_domain_type_idx ON events.contact_requests (domain, request_type, created_at);
CREATE INDEX contact_requests_pending_idx ON events.contact_requests (id) WHERE status = 'pending_issue';
