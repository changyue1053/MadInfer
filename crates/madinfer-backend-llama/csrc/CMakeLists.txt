# madinfer-backend-llama · feature `llama-sys` 的原生构建入口（madai-w16 I-C-15 spike）。
# build.rs 以 `cmake -S <本目录> -B <OUT_DIR>/llama-build` 直接调用（不经 cmake crate：新增 crate 要过 deny.toml 白名单
# 与 vendor/ 离线纪律，两处都不在本卡白名单；本文件只用系统 cmake + 系统编译器）。
# 只编 CPU 路径的静态库：llama + ggml + ggml-base + ggml-cpu + 本目录的 C 桥；Metal / BLAS / Accelerate / OpenMP 全关。
# 三端交叉编译（iOS / Android / OHOS 工具链文件、Metal / Vulkan 子树回填）归 I4d，本文件不承诺。
cmake_minimum_required(VERSION 3.14...3.28)
project(madinfer_llama_sys C CXX)

set(MADINFER_LLAMA_CPP_DIR "${CMAKE_CURRENT_SOURCE_DIR}/../../../third_party/llama.cpp"
    CACHE PATH "vendored llama.cpp 子树（钉版事实见 third_party/PINS.toml）")

# 钉版事实：与 third_party/PINS.toml [llama_cpp] 同值；上游 build-info.cmake 只在未定义时才用 git 猜，
# 这里显式给定，免得把 MadInfer 仓的 HEAD 误写进 llama 的版本串。tests/native.rs 以运行期读数核对 build number。
set(LLAMA_BUILD_NUMBER 10883)
set(LLAMA_BUILD_COMMIT "91f6a6cf361385700bbe15981f0f39909df77498")
# 上游 ggml/CMakeLists.txt 无条件在自己的源目录跑 `git rev-parse` / `git diff-index`：子树在 MadInfer 仓内，会把本仓 HEAD
# 与 "-dirty" 写进 ggml_commit() 串——产物随工作树脏净漂移。把它的 git 换成 /usr/bin/true（输出空 → "unknown"，退出 0 → 不脏）。
if(EXISTS "/usr/bin/true")
    set(GIT_EXE "/usr/bin/true" CACHE FILEPATH "vendored ggml 不探测 git" FORCE)
endif()

set(BUILD_SHARED_LIBS OFF CACHE BOOL "" FORCE)
set(CMAKE_POSITION_INDEPENDENT_CODE ON)           # 静态库最终进 madinfer-ffi 的 cdylib，须 PIC
set(CMAKE_ARCHIVE_OUTPUT_DIRECTORY "${CMAKE_BINARY_DIR}/lib")   # 全部 .a 落一处，build.rs 只给一条 link-search

# 上游子项目开关：非 standalone 时这些缺省已 OFF；显式钉住，防上游改缺省。
set(LLAMA_BUILD_COMMON   OFF CACHE BOOL "" FORCE)
set(LLAMA_BUILD_TESTS    OFF CACHE BOOL "" FORCE)
set(LLAMA_BUILD_TOOLS    OFF CACHE BOOL "" FORCE)
set(LLAMA_BUILD_EXAMPLES OFF CACHE BOOL "" FORCE)
set(LLAMA_BUILD_SERVER   OFF CACHE BOOL "" FORCE)
set(LLAMA_BUILD_APP      OFF CACHE BOOL "" FORCE)
set(LLAMA_ALL_WARNINGS   OFF CACHE BOOL "" FORCE)  # 第三方告警不进我们的构建日志

# ggml：纯 CPU 路径。GGML_NATIVE OFF = 不按构建机 CPU 特化（同 tag + 同开关 → 同产物，金样可跨同架构机器复现）。
set(GGML_METAL      OFF CACHE BOOL "" FORCE)
set(GGML_BLAS       OFF CACHE BOOL "" FORCE)
set(GGML_ACCELERATE OFF CACHE BOOL "" FORCE)
set(GGML_OPENMP     OFF CACHE BOOL "" FORCE)
set(GGML_NATIVE     OFF CACHE BOOL "" FORCE)
set(GGML_CCACHE     OFF CACHE BOOL "" FORCE)

add_subdirectory("${MADINFER_LLAMA_CPP_DIR}" llama.cpp EXCLUDE_FROM_ALL)

# C 桥：MadInfer 侧最小 C 面，只让标量与不透明指针跨 Rust 边界（llama.h 的 by-value 结构在此处消化）。
add_library(madinfer_llama_bridge STATIC madinfer_llama_bridge.c)
target_include_directories(madinfer_llama_bridge PRIVATE "${CMAKE_CURRENT_SOURCE_DIR}")
target_link_libraries(madinfer_llama_bridge PUBLIC llama)
target_compile_definitions(madinfer_llama_bridge PRIVATE MADINFER_LLAMA_BUILD_NUMBER=${LLAMA_BUILD_NUMBER})
target_compile_features(madinfer_llama_bridge PRIVATE c_std_11)
target_compile_options(madinfer_llama_bridge PRIVATE -Wall -Wextra -Werror)
