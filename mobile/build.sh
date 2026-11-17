#!/usr/bin/env bash
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
profile="${1:-phone}"
: "${ANDROID_HOME:=$HOME/Library/Android/sdk}"
if [ -z "${ANDROID_NDK_HOME:-}" ]; then
    ANDROID_NDK_HOME="$(ls -d "$ANDROID_HOME"/ndk/* | sort -V | tail -1)"
fi
export ANDROID_HOME ANDROID_NDK_HOME
cd "$here"
cargo ndk -t arm64-v8a --platform 26 build --profile "$profile"
strip="$(ls "$ANDROID_NDK_HOME"/toolchains/llvm/prebuilt/*/bin/llvm-strip | head -1)"
mkdir -p android/app/src/main/jniLibs/arm64-v8a
"$strip" --strip-all -o android/app/src/main/jniLibs/arm64-v8a/libdrostje_mobile.so \
    "target/aarch64-linux-android/$profile/libdrostje_mobile.so"
cd android
./gradlew --quiet assembleDebug
echo "$here/android/app/build/outputs/apk/debug/app-debug.apk"
