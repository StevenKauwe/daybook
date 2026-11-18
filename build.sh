#!/bin/zsh
set -eu
cd "${0:A:h}"
python3 scripts/vendor_speechworks.py --ensure
export RUSTC_WRAPPER=
export CARGO_BUILD_JOBS=1
# Keep a consistent local signing identity across updates; never silently fall back.
daybook_sign_identity="${DAYBOOK_SIGN_IDENTITY:-}"
if [[ -z "$daybook_sign_identity" && -f data/signing-identity.txt ]]; then
    daybook_sign_identity="$(<data/signing-identity.txt)"
    [[ -n "$daybook_sign_identity" ]] || { print -u2 'Saved signing identity is empty; fix data/signing-identity.txt.'; exit 1; }
fi
if [[ -z "$daybook_sign_identity" ]]; then
    print -u2 'No stable signing identity configured; this development build uses ad-hoc signing.'
    daybook_sign_identity='-'
fi

cargo build --release --locked
cargo build --release --locked --manifest-path native-rust/Cargo.toml
cargo build --release --locked --manifest-path ui-rust/Cargo.toml
daybook_bundle="${DAYBOOK_DIST_DIR:-$PWD/dist}/Daybook.app"
mkdir -p "$daybook_bundle"/Contents/MacOS
cp target/release/daybook-local "$daybook_bundle"/Contents/MacOS/daybook
cp native-rust/target/release/daybook-native "$daybook_bundle"/Contents/MacOS/daybook-native
if [[ "${DAYBOOK_COPY_DEV_HELPERS:-1}" == 1 ]]; then cp native-rust/target/release/daybook-native native/daybook-native; fi
mkdir -p "$daybook_bundle/Contents/Helpers/Daybook Controls.app/Contents/MacOS"
cp ui-rust/target/release/daybook-ui "$daybook_bundle/Contents/Helpers/Daybook Controls.app/Contents/MacOS/daybook-ui"
cp native/Controls-Info.plist "$daybook_bundle/Contents/Helpers/Daybook Controls.app/Contents/Info.plist"
rm -f "$daybook_bundle"/Contents/MacOS/daybook-ui
if [[ "${DAYBOOK_COPY_DEV_HELPERS:-1}" == 1 ]]; then cp ui-rust/target/release/daybook-ui native/daybook-ui; fi
cp native/Info.plist "$daybook_bundle"/Contents/Info.plist
mkdir -p "$daybook_bundle"/Contents/Resources "$daybook_bundle/Contents/Helpers/Daybook Controls.app/Contents/Resources"
cp assets/Daybook.icns "$daybook_bundle"/Contents/Resources/Daybook.icns
cp assets/Daybook.icns "$daybook_bundle/Contents/Helpers/Daybook Controls.app/Contents/Resources/Daybook.icns"
python3 - "$daybook_bundle/Contents/Resources/Build.json" <<'PYBUILD'
import json,sys,time,datetime
with open(sys.argv[1],"w") as f:
    json.dump({"id":str(time.time_ns()),"built_at":datetime.datetime.now(datetime.timezone.utc).isoformat(),"source":"local checkout"},f)
PYBUILD
codesign --force --sign "$daybook_sign_identity" "$daybook_bundle"/Contents/MacOS/daybook-native
codesign --force --sign "$daybook_sign_identity" "$daybook_bundle/Contents/Helpers/Daybook Controls.app"
codesign --force --sign "$daybook_sign_identity" "$daybook_bundle"
printf 'Built Rust-only application %s\n' "$daybook_bundle"
