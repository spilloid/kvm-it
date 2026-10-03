# Contributing

## Workflow

Branch → PR (`.github/PULL_REQUEST_TEMPLATE.md`) → merge. Keep `main` buildable: `scripts/fw.sh test` and
`scripts/fw.sh build` must pass.

## Documentation is load-bearing

Treat docs drift as a bug. A change that alters behaviour updates its docs in the same commit, and a release
includes a docs step. Version lives in `VERSION`, `firmware/CMakeLists.txt` (`PROJECT_VER`) and `CHANGELOG.md`;
keep them equal and keep the README status table honest.

## Honesty rule

Never describe something as working unless it was run. Hardware behaviour is "verified" only when someone
ran it on a physical board. Software-only checks are labelled as such in the PR evidence.

## Secrets and test data

Never put real credentials, Wi-Fi passwords or customer data in code, fixtures, logs or screenshots. Typed
text and secret macro contents must never reach a log line (see docs/security.md).

## Reviews

Non-trivial changes get an adversarial review by a model other than the author; reproduce each finding
against source before accepting it, and record the round in `docs/dev-process.md`.
