#!/usr/bin/env bash
# Prepare and validate Aurora Dict's precompiled llama-server sidecar.
# Application builds only consume the resulting binary. Source compilation is
# an explicit maintainer/CI fallback and never runs automatically on a local PC.

set -euo pipefail

target="${1:-}"
case "$target" in
  aarch64-apple-darwin)
    output_name="llama-server-aarch64-apple-darwin"
    ;;
  x86_64-apple-darwin)
    output_name="llama-server-x86_64-apple-darwin"
    ;;
  x86_64-pc-windows-msvc)
    output_name="llama-server-x86_64-pc-windows-msvc.exe"
    ;;
  x86_64-unknown-linux-gnu)
    output_name="llama-server-x86_64-unknown-linux-gnu"
    ;;
  *)
    echo "Usage: $0 {aarch64-apple-darwin|x86_64-apple-darwin|x86_64-pc-windows-msvc|x86_64-unknown-linux-gnu}" >&2
    exit 2
    ;;
esac

root_dir="$(cd "$(dirname "$0")/.." && pwd)"
output_dir="$root_dir/src-tauri/binaries"
output_path="$output_dir/$output_name"

verify_runtime_dependencies() {
  local binary="$1"
  local dependencies=""
  case "$target" in
    *-apple-darwin)
      command -v otool >/dev/null 2>&1 || { echo "otool is required to validate the macOS sidecar" >&2; return 1; }
      dependencies="$(otool -L "$binary")"
      ;;
    x86_64-unknown-linux-gnu)
      if command -v ldd >/dev/null 2>&1; then
        dependencies="$(ldd "$binary" 2>&1 || true)"
      fi
      ;;
    x86_64-pc-windows-msvc)
      if command -v dumpbin >/dev/null 2>&1; then
        dependencies="$(dumpbin /dependents "$binary")"
      elif command -v objdump >/dev/null 2>&1; then
        dependencies="$(objdump -p "$binary")"
      fi
      ;;
  esac

  local normalized_dependencies
  normalized_dependencies="$(printf '%s' "$dependencies" | tr '[:upper:]' '[:lower:]')"
  case "$normalized_dependencies" in
    *libllama*|*llama.dll*|*libggml*|*ggml.dll*)
      echo "The precompiled llama-server is not self-contained. It still requires libllama/libggml:" >&2
      printf '%s\n' "$dependencies" >&2
      return 1
      ;;
  esac
}

verify_native_execution() {
  local binary="$1"
  local native_target=""
  case "$(uname -s 2>/dev/null)-$(uname -m 2>/dev/null)" in
    Darwin-arm64) native_target="aarch64-apple-darwin" ;;
    Darwin-x86_64) native_target="x86_64-apple-darwin" ;;
    Linux-x86_64) native_target="x86_64-unknown-linux-gnu" ;;
  esac
  if [[ "$target" == "$native_target" ]] && ! "$binary" --version >/dev/null 2>&1; then
    echo "The precompiled llama-server cannot run on this machine. Replace it with a self-contained $target binary." >&2
    return 1
  fi
}

prepare_precompiled_binary() {
  local input="$1"
  [[ -f "$input" ]] || { echo "Precompiled llama-server does not exist: $input" >&2; return 1; }
  verify_runtime_dependencies "$input"
  mkdir -p "$output_dir"
  if [[ "$input" != "$output_path" ]]; then
    cp "$input" "$output_path"
  fi
  chmod +x "$output_path" 2>/dev/null || true
  verify_runtime_dependencies "$output_path"
  verify_native_execution "$output_path"
  echo "Prepared self-contained llama.cpp runtime: $output_path"
}

precompiled="${AURORA_LLAMA_SERVER_PREBUILT:-}"
if [[ -n "$precompiled" ]]; then
  prepare_precompiled_binary "$precompiled"
  exit 0
fi

if [[ "${AURORA_BUILD_LLAMA_FROM_SOURCE:-0}" != "1" ]]; then
  if [[ -f "$output_path" ]]; then
    prepare_precompiled_binary "$output_path"
    exit 0
  fi
  echo "No precompiled llama-server was found for $target." >&2
  echo "Place a self-contained binary at $output_path or set AURORA_LLAMA_SERVER_PREBUILT." >&2
  echo "Maintainer/CI source builds must be explicitly enabled with AURORA_BUILD_LLAMA_FROM_SOURCE=1." >&2
  exit 2
fi

# Explicit maintainer/CI fallback: build a pinned, self-contained binary.
cache_dir="${TMPDIR:-/tmp}/aurora-dict-llama.cpp"
source_dir="$cache_dir/source"
build_dir="$cache_dir/build-$target"
source_version="b5401"

command -v cmake >/dev/null 2>&1 || { echo "cmake is required" >&2; exit 2; }
command -v git >/dev/null 2>&1 || { echo "git is required" >&2; exit 2; }
mkdir -p "$cache_dir" "$output_dir"

local_source="${AURORA_LLAMA_CPP_SOURCE:-}"
if [[ -z "$local_source" ]]; then
  for candidate in "$root_dir/vendor/llama.cpp" "$root_dir/third_party/llama.cpp" "$root_dir/llama.cpp"; do
    if [[ -f "$candidate/CMakeLists.txt" ]]; then
      local_source="$candidate"
      break
    fi
  done
fi
if [[ -n "$local_source" ]]; then
  [[ -f "$local_source/CMakeLists.txt" ]] || { echo "AURORA_LLAMA_CPP_SOURCE is not a llama.cpp source directory: $local_source" >&2; exit 2; }
  source_dir="$(cd "$local_source" && pwd)"
  echo "Using local llama.cpp source: $source_dir"
else
  cmake -E remove_directory "$source_dir"
  git clone --depth 1 --branch "$source_version" https://github.com/ggml-org/llama.cpp.git "$source_dir"
fi

case "$build_dir" in
  "$cache_dir"/build-*) cmake -E remove_directory "$build_dir" ;;
  *) echo "Refusing to clean unexpected build directory: $build_dir" >&2; exit 1 ;;
esac

cmake_args=(
  -S "$source_dir"
  -B "$build_dir"
  -DCMAKE_BUILD_TYPE=Release
  -DBUILD_SHARED_LIBS=OFF
  -DGGML_BACKEND_DL=OFF
  -DLLAMA_CURL=OFF
  -DGGML_NATIVE=OFF
  -DGGML_OPENMP=OFF
)
case "$target" in
  aarch64-apple-darwin) cmake_args+=( -DCMAKE_OSX_ARCHITECTURES=arm64 -DCMAKE_OSX_DEPLOYMENT_TARGET=15.0 -DGGML_METAL=ON ) ;;
  x86_64-apple-darwin) cmake_args+=( -DCMAKE_OSX_ARCHITECTURES=x86_64 -DCMAKE_OSX_DEPLOYMENT_TARGET=15.0 -DGGML_METAL=ON ) ;;
esac

cmake "${cmake_args[@]}"
cmake --build "$build_dir" --config Release --target llama-server --parallel "${CMAKE_BUILD_PARALLEL_LEVEL:-4}"

built_binary="$build_dir/bin/llama-server"
if [[ "$target" == *-pc-windows-msvc ]]; then
  built_binary="$build_dir/bin/Release/llama-server.exe"
  [[ -f "$built_binary" ]] || built_binary="$build_dir/bin/llama-server.exe"
fi
[[ -f "$built_binary" ]] || { echo "llama-server was not produced at $built_binary" >&2; exit 1; }
prepare_precompiled_binary "$built_binary"
