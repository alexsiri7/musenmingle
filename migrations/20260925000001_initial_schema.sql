-- Thaleia initial schema. Every object is schema-qualified with `events.`.
-- The `events` schema itself is created by the application bootstrap
-- (src/db.rs) or by ops/sql/create-role.sql, never by a migration, because the
-- migration bookkeeping table (events._sqlx_migrations) must exist first.
--
-- Migrations are APPEND-ONLY: never edit this file once merged.

CREATE TABLE events.sources (
    id               BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    key              TEXT        NOT NULL UNIQUE,
    kind             TEXT        NOT NULL CHECK (kind IN ('api', 'scraper')),
    base_url         TEXT        NOT NULL,
    domain           TEXT        NOT NULL,
    interval_minutes INTEGER     NOT NULL DEFAULT 1440 CHECK (interval_minutes > 0),
    enabled          BOOLEAN     NOT NULL DEFAULT TRUE,
    last_run_at      TIMESTAMPTZ NULL
);

CREATE TABLE events.events (
    id          UUID             PRIMARY KEY DEFAULT gen_random_uuid(),
    title       TEXT             NOT NULL,
    description TEXT             NULL,
    venue_name  TEXT             NULL,
    address     TEXT             NULL,
    lat         DOUBLE PRECISION NULL CHECK (lat BETWEEN -90 AND 90),
    lng         DOUBLE PRECISION NULL CHECK (lng BETWEEN -180 AND 180),
    starts_at   TIMESTAMPTZ      NOT NULL,
    ends_at     TIMESTAMPTZ      NULL,
    is_free     BOOLEAN          NOT NULL DEFAULT FALSE,
    price_min   NUMERIC(10, 2)   NULL,
    price_max   NUMERIC(10, 2)   NULL,
    currency    TEXT             NULL,
    url         TEXT             NULL,
    image_url   TEXT             NULL,
    category    TEXT             NOT NULL
        CHECK (category IN ('exhibition', 'expo', 'community', 'talk', 'workshop')),
    tags        TEXT[]           NOT NULL DEFAULT '{}',
    dedupe_key  TEXT             NOT NULL,
    created_at  TIMESTAMPTZ      NOT NULL DEFAULT now(),
    updated_at  TIMESTAMPTZ      NOT NULL DEFAULT now(),
    CONSTRAINT events_ends_after_start CHECK (ends_at IS NULL OR ends_at >= starts_at),
    CONSTRAINT events_price_range CHECK (price_min IS NULL OR price_max IS NULL OR price_max >= price_min)
);

-- Cross-source merge key (see src/normalise.rs::dedupe_key).
CREATE UNIQUE INDEX events_dedupe_key_idx ON events.events (dedupe_key);
-- Bounding-box queries: `lat BETWEEN .. AND lng BETWEEN ..` (no PostGIS).
CREATE INDEX events_lat_lng_idx ON events.events (lat, lng) WHERE lat IS NOT NULL AND lng IS NOT NULL;
CREATE INDEX events_starts_at_idx ON events.events (starts_at);
-- "What's on now": exhibitions are ranges, so query by end as well.
CREATE INDEX events_ends_at_idx ON events.events (ends_at) WHERE ends_at IS NOT NULL;
CREATE INDEX events_category_idx ON events.events (category);

CREATE TABLE events.event_sources (
    event_id        UUID        NOT NULL REFERENCES events.events (id) ON DELETE CASCADE,
    source_id       BIGINT      NOT NULL REFERENCES events.sources (id) ON DELETE CASCADE,
    source_event_id TEXT        NOT NULL,
    source_url      TEXT        NULL,
    raw             JSONB       NOT NULL,
    first_seen_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_seen_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (source_id, source_event_id)
);

CREATE INDEX event_sources_event_id_idx ON events.event_sources (event_id);

CREATE TABLE events.source_runs (
    id            BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    source_id     BIGINT      NOT NULL REFERENCES events.sources (id) ON DELETE CASCADE,
    started_at    TIMESTAMPTZ NOT NULL,
    finished_at   TIMESTAMPTZ NOT NULL,
    duration_ms   BIGINT      NOT NULL,
    events_found  INTEGER     NOT NULL DEFAULT 0,
    errors        INTEGER     NOT NULL DEFAULT 0,
    error_summary TEXT        NULL,
    ok            BOOLEAN     NOT NULL
);

CREATE INDEX source_runs_source_started_idx ON events.source_runs (source_id, started_at DESC);

CREATE TABLE events.health_issues (
    id                  BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    source_id           BIGINT      NOT NULL REFERENCES events.sources (id) ON DELETE CASCADE,
    github_issue_number BIGINT      NOT NULL,
    reason              TEXT        NOT NULL,
    opened_at           TIMESTAMPTZ NOT NULL DEFAULT now(),
    closed_at           TIMESTAMPTZ NULL
);

-- At most one OPEN issue per source.
CREATE UNIQUE INDEX health_issues_one_open_per_source_idx
    ON events.health_issues (source_id) WHERE closed_at IS NULL;

CREATE TABLE events.site_suggestions (
    id                  BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    url                 TEXT        NOT NULL,
    domain              TEXT        NOT NULL,
    submitter_ip_hash   TEXT        NULL,
    status              TEXT        NOT NULL DEFAULT 'pending'
        CHECK (status IN ('pending', 'accepted', 'rejected', 'duplicate')),
    github_issue_number BIGINT      NULL,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX site_suggestions_domain_idx ON events.site_suggestions (domain);
