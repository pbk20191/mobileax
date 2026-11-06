#!/usr/bin/env bash
# Fetches Appium's prebuilt WebDriverAgent for the iOS Simulator, on a Mac.
#
#   scripts/ios-wda-sim.sh [--version vX.Y.Z] [--out DIR]
#
# The simulator build (github.com/appium/WebDriverAgent releases,
# WebDriverAgentRunner-Build-Sim-<arch>.zip) embeds XCTest, so it needs no
# signing and starts with `simctl launch`:
#   mobax-ios --sim wda --app DIR/WebDriverAgentRunner-Runner.app
set -euo pipefail

version= out=build/ios-wda-sim
while [ $# -gt 0 ]; do
  case "$1" in
    --version) version=$2; shift 2 ;;
    --out) out=$2; shift 2 ;;
    *) sed -n '2,10p' "$0"; exit 2 ;;
  esac
done

case "$(uname -m)" in
  arm64) arch=arm64 ;;
  x86_64) arch=x86_64 ;;
  *) echo "unsupported host $(uname -m)" >&2; exit 1 ;;
esac
rm -rf "$out" && mkdir -p "$out"
gh release download ${version:+"$version"} -R appium/WebDriverAgent \
  -p "WebDriverAgentRunner-Build-Sim-$arch.zip" -D "$out"
unzip -q "$out/WebDriverAgentRunner-Build-Sim-$arch.zip" -d "$out"
echo "$out/WebDriverAgentRunner-Runner.app"
