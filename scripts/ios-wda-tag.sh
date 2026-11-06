#!/usr/bin/env bash
# Prints the appium/appium-ios release tag of WebDriverAgent version $1
# (X.Y.Z, a leading v is fine), or of the newest one. WDA moved there from
# appium/WebDriverAgent, whose last release is v16.15.3.
set -euo pipefail

if [ -n "${1:-}" ]; then
  echo "appium-webdriveragent@${1#v}"
  exit 0
fi
tag=$(gh release list -R appium/appium-ios -L 200 --json tagName \
  -q '[.[].tagName | select(startswith("appium-webdriveragent@"))][0]')
[ -n "$tag" ] || { echo "no appium-webdriveragent release in appium/appium-ios" >&2; exit 1; }
echo "$tag"
