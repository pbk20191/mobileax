#!/usr/bin/env bash
# Signs Appium's WebDriverAgent runner for one iPhone, on a Mac. Two ways:
#
#   scripts/ios-wda-sign.sh --identity SHA1|NAME --profile FILE.mobileprovision \
#                           [--bundle-id ID] [--version X.Y.Z] [--out DIR]
#   scripts/ios-wda-sign.sh --xcode --udid UDID [--team TEAM_ID] \
#                           [--bundle-id ID] [--version X.Y.Z] [--out DIR]
#
# With --profile, the prebuilt runner from github.com/appium/appium-ios
# releases (appium-webdriveragent@X.Y.Z, WebDriverAgentRunner-Runner.zip,
# unsigned) is re-signed. The
# profile must list the iPhone's UDID and cover ID.xctrunner (a wildcard App ID
# does). `security find-identity -v -p codesigning` lists identities.
#
# With --xcode, Xcode builds the runner from that release's source and makes
# the profile itself, which is the only way a free Apple ID gets one (the
# developer portal is for paid accounts). Add the Apple ID in Xcode › Settings ›
# Accounts first, and connect and unlock the iPhone: Xcode registers it. Free
# accounts: profiles last 7 days (run this again after), 3 devices, 10 App IDs
# a week, and the bundle id must be unused by anyone else, so the default here
# has the team in it. On the iPhone, trust the developer once in Settings ›
# General › VPN & Device Management.
#
# Install the result with `mobax-ios install DIR/WebDriverAgentRunner-Runner.app`.
# On Windows or Linux, go-ios signs instead:
#   ios ui install wda --p12file … --profile … --path …
set -euo pipefail

usage() { sed -n '2,26p' "$0"; exit 2; }
identity= profile= xcode= udid= team= bundle= version= out=build/ios-wda
while [ $# -gt 0 ]; do
  case "$1" in
    --identity) identity=$2; shift 2 ;;
    --profile) profile=$2; shift 2 ;;
    --xcode) xcode=1; shift ;;
    --udid) udid=$2; shift 2 ;;
    --team) team=$2; shift 2 ;;
    --bundle-id) bundle=$2; shift 2 ;;
    --version) version=$2; shift 2 ;;
    --out) out=$2; shift 2 ;;
    *) usage ;;
  esac
done

if [ -n "$xcode" ]; then
  [ -n "$udid" ] || usage
  if [ -z "$team" ]; then
    # The team is the OU of the Apple Development certificate Xcode made.
    teams=$(security find-certificate -a -c "Apple Development" -p 2>/dev/null |
      awk '/BEGIN/{c=""} {c=c $0 "\n"} /END/{printf "%s", c | "openssl x509 -noout -subject"; close("openssl x509 -noout -subject")}' |
      sed -n 's/.*OU *= *\([A-Z0-9]*\).*/\1/p' | sort -u)
    case $(printf '%s' "$teams" | grep -c .) in
      1) team=$teams ;;
      0) echo "no Apple Development certificate yet; pass --team (Xcode › Settings › Accounts shows it)" >&2; exit 1 ;;
      *) echo "several teams ($(echo $teams)); pass --team" >&2; exit 1 ;;
    esac
  fi
  bundle=${bundle:-com.mobileax.$(printf '%s' "$team" | tr 'A-Z' 'a-z').WebDriverAgentRunner}

  tag=$("$(dirname "$0")/ios-wda-tag.sh" "$version")
  rm -rf "$out" && mkdir -p "$out/src"
  gh release download "$tag" -R appium/appium-ios --archive=tar.gz -O "$out/src.tar.gz"
  tar -xzf "$out/src.tar.gz" -C "$out/src" --strip-components 3 '*/packages/webdriveragent/*'
  xcodebuild build-for-testing -quiet \
    -project "$out/src/WebDriverAgent.xcodeproj" -scheme WebDriverAgentRunner \
    -destination "id=$udid" -derivedDataPath "$out/derived" \
    -allowProvisioningUpdates -allowProvisioningDeviceRegistration \
    DEVELOPMENT_TEAM="$team" CODE_SIGN_STYLE=Automatic CODE_SIGN_IDENTITY="Apple Development" \
    PRODUCT_BUNDLE_IDENTIFIER="$bundle" COMPILER_INDEX_STORE_ENABLE=NO
  app="$out/WebDriverAgentRunner-Runner.app"
  cp -R "$out/derived/Build/Products/Debug-iphoneos/WebDriverAgentRunner-Runner.app" "$app"
  rm -rf "$out/src" "$out/src.tar.gz" "$out/derived"
  codesign --verify --deep --strict "$app"
  expires=$(security cms -D -i "$app/embedded.mobileprovision" |
    plutil -extract ExpirationDate raw -o - -)
  echo "signed $app ($bundle.xctrunner, team $team, profile expires $expires)"
  exit 0
fi

[ -n "$identity" ] && [ -n "$profile" ] || usage
bundle=${bundle:-com.mobileax.WebDriverAgentRunner}

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

tag=$("$(dirname "$0")/ios-wda-tag.sh" "$version")
rm -rf "$out" && mkdir -p "$out"
gh release download "$tag" -R appium/appium-ios -p WebDriverAgentRunner-Runner.zip -D "$out"
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
