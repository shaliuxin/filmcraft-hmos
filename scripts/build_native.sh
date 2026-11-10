#!/usr/bin/env bash
# Build the HarmonyOS native libraries (every ABI in FC_NATIVE_TARGETS) before hvigor/CMake.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
if [[ -f "$ROOT/scripts/dev.local.env" ]]; then source "$ROOT/scripts/dev.local.env"; fi
# 界面中文：内嵌 craft-i18n 的 zh-hans/zh-hant catalog（上游 crates/ui-egui/build.rs 读 CRAFT_I18N_DIR，
# 未设则「English only」）。Windows/MSYS 须传带盘符的绝对路径（pwd -W），否则被判为相对路径而拒绝。
CRAFT_I18N_DIR="${CRAFT_I18N_DIR:-}"
if [[ -z "$CRAFT_I18N_DIR" && -d "$ROOT/external/craft-i18n" && -f "$ROOT/external/craft-i18n/locales/zh-hans/wordcraft.tsv" ]]; then
  CRAFT_I18N_DIR="$(cd "$ROOT/external/craft-i18n" && { pwd -W 2>/dev/null || pwd; })"
fi
if [[ -n "$CRAFT_I18N_DIR" ]]; then
  export CRAFT_I18N_DIR
  echo "CRAFT_I18N_DIR=$CRAFT_I18N_DIR"
else
  echo "WARN: craft-i18n checkout not found (external/craft-i18n); the interface will be English only" >&2
fi
SDK="${FC_NATIVE_SDK:-${DEVECO_SDK_HOME:-${HOS_SDK_HOME:-/Applications/DevEco-Studio.app/Contents/sdk}}/default/openharmony/native}"
# CI SDK locations may already point at openharmony, or at its native directory.
if [[ ! -d "$SDK/llvm" ]]; then
  for candidate in "${DEVECO_SDK_HOME:-}/openharmony/native" "${DEVECO_SDK_HOME:-}/native" "${DEVECO_SDK_HOME:-}"; do
    if [[ -d "$candidate/llvm" && -d "$candidate/sysroot" ]]; then SDK="$candidate"; break; fi
  done
fi
{ [[ -x "$SDK/llvm/bin/clang" || -f "$SDK/llvm/bin/clang.exe" ]]; } || { echo "HarmonyOS native SDK not found: $SDK" >&2; exit 1; }
# One list drives the build: CMake maps the same ABIs (entry/build-profile.json5 abiFilters), and
# verify_native.py checks the packaged set. Both ABIs ship by default so one HAP runs on ARM
# devices and on the x86_64 phone/PC emulator images.
TARGETS="${FC_NATIVE_TARGETS:-aarch64-unknown-linux-ohos x86_64-unknown-linux-ohos}"
PROFILE="${FC_RUST_PROFILE:-release}"
case "$PROFILE" in release) ARGS+=(--release);; debug) ARGS+=(--profile dev);; *) echo 'Profile must be release or debug' >&2; exit 2;; esac
for target in $TARGETS; do
  case "$target" in
    aarch64-unknown-linux-ohos|x86_64-unknown-linux-ohos|armv7-unknown-linux-ohos) ;;
    *) echo "Unsupported target: $target" >&2; exit 2;;
  esac
  [[ -d "$(rustc --print sysroot)/lib/rustlib/$target" ]] || { echo "Install target with: rustup target add $target" >&2; exit 1; }
  suffix="${target//-/_}"
  upper="$(printf '%s' "$suffix" | tr '[:lower:]' '[:upper:]')"
  # Windows: the `*-unknown-linux-ohos-clang` tools are POSIX sh wrappers that Rust cannot spawn
  # (os error 193). Drive both the cc-crate C probe and the final link through clang.exe directly,
  # and carry the OHOS target flags via CFLAGS (cc-rs already injects its own --target too).
  # macOS/Linux keep the wrapper (it bakes target+sysroot).
  #
  # Two traps in CFLAGS on Windows:
  #   1) cc-rs WORD-SPLITS CFLAGS, so a spaced --sysroot ("C:\Program Files\...") breaks.
  #   2) cc-rs (a Rust process) passes CFLAGS VERBATIM — unlike bash/MSYS it does NOT translate
  #      `/c/...` to `C:/...`, so the sysroot must already be a Windows-form path.
  # Fix for both: a space-free directory junction C:\ohosnative, referenced as C:/ohosnative
  # (Windows form, no spaces) in both CC and CFLAGS.
  cflags=""
  if [[ -x "$SDK/llvm/bin/clang.exe" ]]; then
    base_win="C:/ohosnative"
    if [[ ! -d "/c/ohosnative/llvm" ]]; then
      win_target="$(cygpath -w "$SDK" 2>/dev/null || printf '%s' "$SDK")"
      powershell -NoProfile -Command "New-Item -ItemType Junction -Path 'C:\\ohosnative' -Target '$win_target' | Out-Null" \
        || { echo "Failed to create junction C:\\ohosnative -> $SDK" >&2; exit 1; }
    fi
    compiler="$base_win/llvm/bin/clang.exe"
    case "$target" in
      aarch64-unknown-linux-ohos) clang_target="aarch64-linux-ohos";;
      x86_64-unknown-linux-ohos)  clang_target="x86_64-linux-ohos";;
      armv7-unknown-linux-ohos)   clang_target="armv7-linux-ohos";;
    esac
    cflags="--target=$clang_target --sysroot=$base_win/sysroot -D__MUSL__"
  else
    compiler="$SDK/llvm/bin/$target-clang"
  fi
  if [[ -n "$cflags" ]]; then
    env "CC_${suffix}=$compiler" "CFLAGS_${suffix}=$cflags" "CFLAGS=$cflags" \
        "AR_${suffix}=$SDK/llvm/bin/llvm-ar" \
        "CARGO_TARGET_${upper}_LINKER=$compiler" \
        cargo build --locked --manifest-path "$ROOT/native/rust/Cargo.toml" --target "$target" "${ARGS[@]}"
  else
    env "CC_${suffix}=$compiler" "AR_${suffix}=$SDK/llvm/bin/llvm-ar" \
        "CARGO_TARGET_${upper}_LINKER=$compiler" \
        cargo build --locked --manifest-path "$ROOT/native/rust/Cargo.toml" --target "$target" "${ARGS[@]}"
  fi
done
