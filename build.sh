#!/usr/bin/env bash
# Builds build/mobaxd.apk and build/mobaxd-instr.apk. The real pipeline lives in build.gradle.kts.
set -euo pipefail
cd "$(dirname "$0")"
exec ./gradlew --quiet mobaxApk "$@"
