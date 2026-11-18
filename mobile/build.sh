#!/usr/bin/env bash
set -euo pipefail
if [[ "${1:-}" == --help || "${1:-}" == -h ]]; then
    cat <<'HELP'
Usage: bash mobile/build.sh [profile] [debug|release]

Defaults: phone debug.
Requires the Android SDK and NDK, Java 17, the aarch64-linux-android
Rust target and cargo-ndk. Builds use mobile/Cargo.lock.

ANDROID_HOME selects the SDK (defaults to ~/Library/Android/sdk).
ANDROID_NDK_HOME selects the NDK (defaults to the latest installed version).
ANDROID_VERSION_NAME and ANDROID_VERSION_CODE override APK version metadata.

Examples:
  bash mobile/build.sh
  bash mobile/build.sh release release

Debug APK: mobile/android/app/build/outputs/apk/debug/app-debug.apk
Release APK: mobile/android/app/build/outputs/apk/release/app-release-unsigned.apk
Local release APKs are unsigned.

GitHub releases:
  Push android-vMAJOR.MINOR.PATCH, optionally followed by -rc.N.
  The Release Android workflow signs and verifies the APK, then publishes
  it with SHA256SUMS and generated release notes. RC tags create prereleases.
  Each release must have a higher version than the previous one.
  Limits: major 0..209, minor 0..99, patch 0..999, rc 1..98.

One-time signing setup:
  Outside the repository, create and securely back up a release keystore:
    keytool -genkeypair -keystore drostje-release.jks -alias drostje \
      -keyalg RSA -keysize 4096 -validity 10000
  Keep using the same key for all updates.

  Under GitHub Settings > Secrets and variables > Actions, add:
    ANDROID_KEYSTORE_BASE64    Base64-encoded contents of the keystore
    ANDROID_KEYSTORE_PASSWORD  Keystore password
    ANDROID_KEY_ALIAS          Key alias (drostje in the example)
    ANDROID_KEY_PASSWORD       Key password

  Encode the keystore with:
    base64 < drostje-release.jks | tr -d '\n'
  Copy the result only into the GitHub secret, never into the repository
  or a chat. Do not share the keystore or passwords.

Devices with a debug-signed installation must uninstall it before
installing the first release. This removes local data and pairing.
HELP
    exit 0
fi
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
profile="${1:-phone}"
variant="${2:-debug}"
case "$variant" in
    debug) task="assembleDebug"; apk="app-debug.apk" ;;
    release) task="assembleRelease"; apk="app-release-unsigned.apk" ;;
    *) echo "Build variant must be debug or release" >&2; exit 1 ;;
esac
: "${ANDROID_HOME:=$HOME/Library/Android/sdk}"
if [ -z "${ANDROID_NDK_HOME:-}" ]; then
    ANDROID_NDK_HOME="$(ls -d "$ANDROID_HOME"/ndk/* | sort -V | tail -1)"
fi
export ANDROID_HOME ANDROID_NDK_HOME
cd "$here"
printf 'Building Android native library with profile %s\n' "$profile"
cargo ndk -t arm64-v8a --platform 30 build --locked --profile "$profile"
printf 'Preparing Android native library\n'
strip="$(ls "$ANDROID_NDK_HOME"/toolchains/llvm/prebuilt/*/bin/llvm-strip | head -1)"
mkdir -p android/app/src/main/jniLibs/arm64-v8a
"$strip" --strip-all -o android/app/src/main/jniLibs/arm64-v8a/libdrostje_mobile.so \
    "target/aarch64-linux-android/$profile/libdrostje_mobile.so"
cd android
printf 'Packaging Android APK with %s\n' "$task"
./gradlew --no-daemon --console=plain --info --stacktrace "$task"
echo "$here/android/app/build/outputs/apk/$variant/$apk"
