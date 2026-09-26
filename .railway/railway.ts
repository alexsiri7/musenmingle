// Railway Infrastructure as Code for the `thaleia` project (replaces the
// retired railway.toml config-as-code). Railway does not read this file on
// deploy: preview with `railway config plan`, then `railway config apply`
// (Railway CLI >= 5.42.1). See README.md "Deployment (Railway)".
//
// Variables are preserve()d so apply keeps the values Railway already holds;
// never put values or secrets here. When you add a variable in the dashboard,
// add its preserve() line here too.
import { defineRailway, github, preserve, project, service } from "railway/iac";

const source = github("alexsiri7/thaleia", { branch: "main" });
const build = { builder: "DOCKERFILE", dockerfilePath: "Dockerfile" } as const;
const regions = { "europe-west4-drams3a": 1 };

export default defineRailway(() => {
  const api = service("thaleia-api", {
    source,
    build,
    start: "thaleia-api",
    healthcheck: "/healthz",
    healthcheckTimeout: 30,
    deploy: { restartPolicyType: "ON_FAILURE", restartPolicyMaxRetries: 5 },
    regions,
    env: {
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
  const ingest = service("thaleia-ingest", {
    source,
    build,
    start: "thaleia-ingest",
    deploy: { cronSchedule: "*/15 * * * *", restartPolicyType: "NEVER" },
    regions,
    env: {
      DATABASE_URL: preserve(),
      GITHUB_REPO: preserve(),
      RATE_LIMIT_MS: preserve(),
      RATE_LIMIT_OVERRIDES: preserve(),
      RUST_LOG: preserve(),
      SOURCE_TIMEOUT_SECS: preserve(),
    },
  });

  return project("thaleia", { resources: [api, ingest] });
});
