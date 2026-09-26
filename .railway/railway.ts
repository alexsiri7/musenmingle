// Railway Infrastructure as Code for the `musenmingle` project (replaces the
// retired railway.toml config-as-code). Railway does not read this file on
// deploy: preview with `railway config plan`, then `railway config apply`
// (Railway CLI >= 5.42.1). See README.md "Deployment (Railway)".
//
// Variables are preserve()d so apply keeps the values Railway already holds;
// never put values or secrets here. When you add a variable in the dashboard,
// add its preserve() line here too.
import { defineRailway, github, preserve, project, service } from "railway/iac";

const source = github("alexsiri7/musenmingle", { branch: "main" });
const build = { builder: "DOCKERFILE", dockerfilePath: "Dockerfile" } as const;
const regions = { "europe-west4-drams3a": 1 };

export default defineRailway(() => {
  const api = service("musenmingle-api", {
    source,
    build,
    start: "musenmingle-api",
    healthcheck: "/healthz",
    healthcheckTimeout: 30,
    deploy: { restartPolicyType: "ON_FAILURE", restartPolicyMaxRetries: 5 },
    regions,
    env: {
      CANONICAL_HOST: preserve(),
      DATABASE_URL: preserve(),
      GITHUB_REPO: preserve(),
      PORT: preserve(),
      RUST_LOG: preserve(),
      SUGGESTION_IP_SALT: preserve(),
      SUGGESTION_RATE_PER_DAY: preserve(),
      SUGGESTION_RATE_PER_HOUR: preserve(),
      TRUSTED_PROXY_COUNT: preserve(),
    },
  });

  // Runs every 15 minutes and exits; per-source interval_minutes decides what
  // runs on each tick. No healthcheck (not a server) and no restarts (the next
  // tick is the retry).
  const ingest = service("musenmingle-ingest", {
    source,
    build,
    start: "musenmingle-ingest",
    deploy: { cronSchedule: "*/15 * * * *", restartPolicyType: "NEVER" },
    regions,
    env: {
      DATABASE_URL: preserve(),
      ENRICH_DAILY_CAP_USD: preserve(),
      ENRICH_MODEL: preserve(),
      GITHUB_REPO: preserve(),
      GITHUB_TOKEN: preserve(),
      NTFY_TOPIC: preserve(),
      RATE_LIMIT_MS: preserve(),
      RATE_LIMIT_OVERRIDES: preserve(),
      REQUESTY_API_KEY: preserve(),
      RUST_LOG: preserve(),
      SOURCE_TIMEOUT_SECS: preserve(),
      TICKETMASTER_API_KEY: preserve(),
    },
  });

  return project("musenmingle", { resources: [api, ingest] });
});
