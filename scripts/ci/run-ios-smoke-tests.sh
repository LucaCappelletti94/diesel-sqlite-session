#!/usr/bin/env bash
set -euo pipefail

if [[ -z "${RUST_SMOKE_LIB_DIR:-}" ]]; then
  echo "RUST_SMOKE_LIB_DIR must be set"
  exit 1
fi

if [[ ! -f "${RUST_SMOKE_LIB_DIR}/libdiesel_sqlite_session.a" ]]; then
  echo "Rust static library not found at ${RUST_SMOKE_LIB_DIR}/libdiesel_sqlite_session.a"
  exit 1
fi

# Pick the latest available iOS runtime and a widely available iPhone device type.
IOS_RUNTIME=$(
  xcrun simctl list runtimes | \
    awk -F '[()]' '/iOS/ && /available/ && /com.apple.CoreSimulator.SimRuntime.iOS/ { print $2 }' | \
    tail -n 1
)

if [[ -z "${IOS_RUNTIME}" ]]; then
  echo "Could not resolve an available iOS simulator runtime"
  exit 1
fi

IOS_DEVICE_TYPE=$(
  xcrun simctl list devicetypes | \
    awk -F '[()]' '/iPhone 16/ { print $2 }' | \
    head -n 1
)
if [[ -z "${IOS_DEVICE_TYPE}" ]]; then
  IOS_DEVICE_TYPE=$(
    xcrun simctl list devicetypes | \
      awk -F '[()]' '/iPhone 15/ { print $2 }' | \
      head -n 1
  )
fi
if [[ -z "${IOS_DEVICE_TYPE}" ]]; then
  IOS_DEVICE_TYPE=$(
    xcrun simctl list devicetypes | \
      awk -F '[()]' '/iPhone/ { print $2 }' | \
      head -n 1
  )
fi

if [[ -z "${IOS_DEVICE_TYPE}" ]]; then
  echo "Could not resolve an iPhone simulator device type"
  exit 1
fi

SIMULATOR_NAME="diesel-sqlite-session-smoke-${GITHUB_RUN_ID:-local}-${GITHUB_RUN_ATTEMPT:-0}"
SIMULATOR_ID=""

cleanup() {
  if [[ -n "${SIMULATOR_ID}" ]]; then
    xcrun simctl shutdown "${SIMULATOR_ID}" >/dev/null 2>&1 || true
    xcrun simctl delete "${SIMULATOR_ID}" >/dev/null 2>&1 || true
  fi
}
trap cleanup EXIT

SIMULATOR_ID=$(xcrun simctl create "${SIMULATOR_NAME}" "${IOS_DEVICE_TYPE}" "${IOS_RUNTIME}")

# Simulator boot can be flaky on shared runners; retry with a clean restart.
for attempt in 1 2 3; do
  if xcrun simctl boot "${SIMULATOR_ID}" >/dev/null 2>&1; then
    break
  fi

  if [[ "${attempt}" -eq 3 ]]; then
    echo "Failed to boot simulator after ${attempt} attempts"
    exit 1
  fi

  xcrun simctl shutdown "${SIMULATOR_ID}" >/dev/null 2>&1 || true
  killall -9 Simulator >/dev/null 2>&1 || true
  sleep 5

done

xcrun simctl bootstatus "${SIMULATOR_ID}" -b

pushd mobile-tests/ios-smoke >/dev/null

xcodebuild \
  -scheme IOSSmoke-Package \
  -sdk iphonesimulator \
  -destination "id=${SIMULATOR_ID}" \
  -derivedDataPath DerivedData \
  -parallel-testing-enabled NO \
  CODE_SIGNING_ALLOWED=NO CODE_SIGNING_REQUIRED=NO CODE_SIGN_IDENTITY="" \
  build-for-testing

xcodebuild \
  -scheme IOSSmoke-Package \
  -sdk iphonesimulator \
  -destination "id=${SIMULATOR_ID}" \
  -derivedDataPath DerivedData \
  -parallel-testing-enabled NO \
  CODE_SIGNING_ALLOWED=NO CODE_SIGNING_REQUIRED=NO CODE_SIGN_IDENTITY="" \
  test-without-building

popd >/dev/null
