#!/usr/bin/env bash
set -euo pipefail
bash scripts/with-test-services.sh cargo test --offline --test api -- --ignored
npm test --prefix frontend
