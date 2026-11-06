#!/usr/bin/env bash
# Signs Appium's prebuilt WebDriverAgent runner for one iPhone, on a Mac.
#
#   scripts/ios-wda-sign.sh --identity SHA1|NAME --profile FILE.mobileprovision \
#                           [--bundle-id com.example.WebDriverAgentRunner] [--version vX.Y.Z] [--out DIR]
#
# The runner comes from github.com/appium/WebDriverAgent releases
# (WebDriverAgentRunner-Runner.zip, built for devices and unsigned). The
# profile must list the iPhone's UDID and cover the bundle id (a wildcard
# App ID does). `security find-identity -v -p codesigning` lists identities.
#
# Install and start the result with go-ios, which also works on Windows and
# Linux (there, sign with `ios ui install wda --p12file … --profile … --path`):
#   ios install --path=DIR/WebDriverAgentRunner-Runner.app
#   ios ui run wda --bundleid=BUNDLE_ID --test-runner-bundleid=BUNDLE_ID.xctrunner
set -euo pipefail

identity= profile= bundle=com.mobileax.WebDriverAgentRunner version= out=build/ios-wda
while [ $# -gt 0 ]; do
  case "$1" in
    --identity) identity=$2; shift 2 ;;
    --profile) profile=$2; shift 2 ;;
    --bundle-id) bundle=$2; shift 2 ;;
    --version) version=$2; shift 2 ;;
    --out) out=$2; shift 2 ;;
    *) sed -n '2,16p' "$0"; exit 2 ;;
  esac
done
[ -n "$identity" ] && [ -n "$profile" ] || { sed -n '2,16p' "$0"; exit 2; }

# codesign takes any identity whose name merely contains the string, so check
# for an exact SHA-1 or name first.
security find-identity -v -p codesigning | awk -v want="$identity" '
  { name = $0; sub(/^[^"]*"/, "", name); sub(/"[^"]*$/, "", name)
    if ($2 == want || name == want) found = 1 }
  END { exit !found }' || {
  echo "no codesigning identity is exactly \"$identity\"; these are:" >&2
  security find-identity -v -p codesigning >&2
  exit 1
}

decoded=$(mktemp)
trap 'rm -f "$decoded"' EXIT
security cms -D -i "$profile" > "$decoded"
team=$(/usr/libexec/PlistBuddy -c 'Print :Entitlements:com.apple.developer.team-identifier' "$decoded")
appid=$(/usr/libexec/PlistBuddy -c 'Print :Entitlements:application-identifier' "$decoded")
case "$bundle.xctrunner" in
  ${appid#"$team".}) ;;
  *) echo "profile $appid does not cover $bundle.xctrunner" >&2; exit 1 ;;
esac

rm -rf "$out" && mkdir -p "$out"
gh release download ${version:+"$version"} -R appium/WebDriverAgent -p WebDriverAgentRunner-Runner.zip -D "$out"
unzip -q "$out/WebDriverAgentRunner-Runner.zip" -d "$out"
app="$out/WebDriverAgentRunner-Runner.app"
xctest="$app/PlugIns/WebDriverAgentRunner.xctest"
find "$app" -name '*.dSYM' -prune -exec rm -rf {} +

/usr/libexec/PlistBuddy -c "Set :CFBundleIdentifier $bundle.xctrunner" "$app/Info.plist"
/usr/libexec/PlistBuddy -c "Set :CFBundleIdentifier $bundle" "$xctest/Info.plist"
cp "$profile" "$app/embedded.mobileprovision"

cat > "$out/entitlements.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>application-identifier</key><string>$team.$bundle.xctrunner</string>
  <key>com.apple.developer.team-identifier</key><string>$team</string>
  <key>get-task-allow</key><true/>
  <key>keychain-access-groups</key><array><string>$team.*</string></array>
</dict></plist>
EOF

# Inside out: frameworks, then the test bundle, then the app with entitlements.
find "$xctest/Frameworks" -maxdepth 1 -name '*.framework' -exec codesign -f -s "$identity" {} \;
codesign -f -s "$identity" "$xctest"
codesign -f -s "$identity" --entitlements "$out/entitlements.plist" "$app"
codesign --verify --deep --strict "$app"
echo "signed $app ($bundle.xctrunner, team $team)"
