cmake_minimum_required(VERSION 3.5.0)
project(filmcraft_native)
set(CMAKE_CXX_STANDARD 17)
get_filename_component(FC_ROOT "${CMAKE_CURRENT_SOURCE_DIR}/../../../.." ABSOLUTE)
# OHOS_ARCH is the ABI hvigor is building (abiFilters in entry/build-profile.json5). This mapping
# is the only place in the C++ layer that knows about architectures: adding an ABI means adding a
# line here plus the corresponding triple in scripts/build_native.sh and scripts/verify_native.py.
if(OHOS_ARCH STREQUAL "arm64-v8a")
  set(RUST_TARGET aarch64-unknown-linux-ohos)
elseif(OHOS_ARCH STREQUAL "x86_64")
  set(RUST_TARGET x86_64-unknown-linux-ohos)
elseif(OHOS_ARCH STREQUAL "armeabi-v7a")
  set(RUST_TARGET armv7-unknown-linux-ohos)
else()
  message(FATAL_ERROR "FilmCraft does not support OHOS_ARCH=${OHOS_ARCH}")
endif()
if(NOT DEFINED FC_RUST_PROFILE)
  set(FC_RUST_PROFILE release)
endif()
# FC_RUST_TARGET_DIR points at a target directory outside the workspace (CI caches it); it
# must contain <triple>/<profile>/libfilmcraft_hmos.a for the ABI being built.
if(DEFINED FC_RUST_TARGET_DIR AND NOT FC_RUST_TARGET_DIR STREQUAL "")
  set(RUST_LIBRARY "${FC_RUST_TARGET_DIR}/${RUST_TARGET}/${FC_RUST_PROFILE}/libfilmcraft_hmos.a")
else()
  set(RUST_LIBRARY "${FC_ROOT}/native/rust/target/${RUST_TARGET}/${FC_RUST_PROFILE}/libfilmcraft_hmos.a")
endif()
if(NOT EXISTS "${RUST_LIBRARY}")
  message(FATAL_ERROR "Run scripts/build_native.sh before building HAP: ${RUST_LIBRARY}")
endif()
add_library(filmcraft SHARED bridge.cpp)
# FilmCraft 依赖树带 `iana_time_zone`（经 craft-update/rustls 引入），它调用 OHOS TimeService NDK
# API `OH_TimeService_GetTimeZone`（BasicServicesKit/time_service.h，lib `libtime_service_ndk.so`）。
# aarch64 与 x86_64 sysroot 均已验证含该库，缺链会报 undefined symbol。
target_link_libraries(filmcraft PRIVATE "${RUST_LIBRARY}" ace_napi.z ace_ndk.z native_window native_vsync hilog_ndk.z time_service_ndk EGL GLESv3 dl m pthread)

target_link_options(filmcraft PRIVATE "-Wl,--gc-sections" "-Wl,--exclude-libs,ALL")
