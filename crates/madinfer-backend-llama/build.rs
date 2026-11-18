//! madinfer-backend-llama 构建脚本：只在 cargo feature `llama-sys` 下有动作（缺省关 → 零动作、零输出，不带 feature 的
//! 全仓构建 / 测试与基线逐字节不变）。开启后以**系统 cmake** 经 `csrc/CMakeLists.txt` 编 `third_party/llama.cpp`
//! （CPU 路径静态库：llama + ggml + ggml-base + ggml-cpu）与 C 桥 `madinfer_llama_bridge`，再链进本 crate。
//!
//! 不经 cmake crate（madai-w16 I-C-15 判断）：新增 build-dependency 要进 deny.toml 白名单并重跑 `cargo vendor`，两处都不在
//! 白名单；系统 cmake 与系统编译器已是 llama.cpp 自身的构建前提。
//!
//! 三端交叉编译（madai-w17 I-C-17）：按 `TARGET` / `CARGO_CFG_TARGET_OS` 给 cmake 传工具链——
//! - `macos`：宿主，不加参数；
//! - `ios`：`CMAKE_SYSTEM_NAME=iOS`，`TARGET` 含 `-sim` 取 `iphonesimulator` 否则 `iphoneos`，arm64，部署 16.0；
//! - `android`：NDK `build/cmake/android.toolchain.cmake`，`arm64-v8a` / `android-24` / `c++_static`（`ANDROID_NDK_HOME` 缺省 27.2）；
//! - 鸿蒙（`TARGET` 以 `-linux-ohos` 结尾；其 target_os 是 `linux`、target_env 是 `ohos`）：DevEco
//!   `native/build/cmake/ohos.toolchain.cmake`，`OHOS_ARCH=arm64-v8a`（`OHOS_SDK_NATIVE` 缺省 DevEco 内嵌）。
//!
//! 非宿主的 aarch64 Linux 系目标（android / ohos）显式 `GGML_CPU_ARM_ARCH`（缺省 `armv8-a`，`MADINFER_LLAMA_ARM_ARCH` 覆盖）：
//! `GGML_NATIVE=OFF` 时 ggml 不给 `-march`，交叉编译器缺省即 ABI 基线，这里写明免得随工具链缺省漂移。
//!
//! 预编静态库（I4-01 `MADINFER_LLAMA_LIB_DIR` 形态）：环境变量指向含 5 个 `.a` 的目录（`tools/llama/build_targets.sh` 的
//! `target/madinfer-llama/<tag>/<triple>/`）时**不调 cmake**，直接 `rustc-link-search` 该目录；目录缺任一 `.a` 即 panic 点名，不回退。
//!
//! 环境：`CMAKE`（可执行名，缺省 `cmake`）、`MADINFER_LLAMA_CMAKE_JOBS`（并行度，缺省 min(可用核, 8)）、`MADINFER_LLAMA_LIB_DIR`、
//! `ANDROID_NDK_HOME`、`OHOS_SDK_NATIVE`、`MADINFER_LLAMA_ARM_ARCH`。
//! llama.cpp 恒按 Release 编（与 cargo profile 无关）：Debug 形态的 ggml 解码慢一个量级，金样用例跑不动。

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    #[cfg(feature = "llama-sys")]
    llama_sys::build();
}

#[cfg(feature = "llama-sys")]
mod llama_sys {
    use std::env;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    const BRIDGE_TARGET: &str = "madinfer_llama_bridge";
    /// 链接顺序按依赖：桥 → llama → ggml 门面 → CPU 后端 → ggml 基座。
    const STATIC_LIBS: [&str; 5] = [BRIDGE_TARGET, "llama", "ggml", "ggml-cpu", "ggml-base"];

    const DEFAULT_ANDROID_NDK: &str = "Library/Android/sdk/ndk/27.2.12479018";
    const DEFAULT_OHOS_SDK_NATIVE: &str =
        "/Applications/DevEco-Studio.app/Contents/sdk/default/openharmony/native";
    const DEFAULT_ARM_ARCH: &str = "armv8-a";

    fn env_path(key: &str) -> PathBuf {
        match env::var_os(key) {
            Some(value) => PathBuf::from(value),
            None => panic!("build.rs: cargo 未提供环境变量 {key}"),
        }
    }

    fn run(step: &str, command: &mut Command) {
        match command.status() {
            Ok(status) if status.success() => {}
            Ok(status) => panic!("build.rs: llama-sys {step} 失败：{status}（命令 {command:?}）"),
            Err(error) => {
                panic!("build.rs: llama-sys {step} 无法启动 cmake：{error}（命令 {command:?}）")
            }
        }
    }

    fn jobs() -> String {
        if let Ok(explicit) = env::var("MADINFER_LLAMA_CMAKE_JOBS") {
            return explicit;
        }
        let cores = std::thread::available_parallelism().map_or(4, |n| n.get());
        cores.min(8).to_string()
    }

    /// 目标平台（由 cargo 的 `CARGO_CFG_TARGET_OS` 与 `TARGET` 判）。
    enum Platform {
        Macos,
        Ios {
            simulator: bool,
        },
        Android,
        Ohos,
        /// 本卡未承诺的目标：宿主工具链 + libstdc++，不传工具链文件。
        Other,
    }

    fn platform() -> Platform {
        let target = env::var("TARGET").unwrap_or_default();
        // 鸿蒙三元组 aarch64-unknown-linux-ohos 的 target_os 是 `linux`，鸿蒙只落在 target_env（rustc 目标规范），
        // 只看 CARGO_CFG_TARGET_OS 会把它当宿主编——此处先按三元组后缀判鸿蒙，再判 os。
        if target.ends_with("-linux-ohos") {
            return Platform::Ohos;
        }
        match env::var("CARGO_CFG_TARGET_OS").as_deref() {
            Ok("macos") => Platform::Macos,
            Ok("ios") => Platform::Ios {
                simulator: target.ends_with("-sim"),
            },
            Ok("android") => Platform::Android,
            _ => {
                println!("cargo:warning=llama-sys: 目标 {target} 不在三端清单，按宿主工具链编");
                Platform::Other
            }
        }
    }

    fn home() -> PathBuf {
        env::var_os("HOME").map_or_else(|| PathBuf::from("/"), PathBuf::from)
    }

    fn arm_arch() -> String {
        env::var("MADINFER_LLAMA_ARM_ARCH").unwrap_or_else(|_| DEFAULT_ARM_ARCH.to_owned())
    }

    /// 按目标给 cmake 的 configure 参数（宿主 macOS 为空）。交叉目标一律 `CMAKE_TRY_COMPILE_TARGET_TYPE=STATIC_LIBRARY`：
    /// 只编静态库，configure 期的 try_compile 不该去链可执行（iOS 无 main 入口、Android / OHOS 缺 sysroot 运行时时会假红）。
    fn toolchain_args(platform: &Platform) -> Vec<String> {
        let cross = vec!["-DCMAKE_TRY_COMPILE_TARGET_TYPE=STATIC_LIBRARY".to_owned()];
        match platform {
            Platform::Macos | Platform::Other => Vec::new(),
            Platform::Ios { simulator } => {
                let sysroot = if *simulator {
                    "iphonesimulator"
                } else {
                    "iphoneos"
                };
                let mut args = cross;
                args.extend([
                    "-DCMAKE_SYSTEM_NAME=iOS".to_owned(),
                    format!("-DCMAKE_OSX_SYSROOT={sysroot}"),
                    "-DCMAKE_OSX_ARCHITECTURES=arm64".to_owned(),
                    "-DCMAKE_OSX_DEPLOYMENT_TARGET=16.0".to_owned(),
                    // csrc/CMakeLists.txt 已钉 OFF；此处再显式一次是 iOS 上游缺省会开 Metal / Accelerate。
                    "-DGGML_METAL=OFF".to_owned(),
                    "-DGGML_ACCELERATE=OFF".to_owned(),
                ]);
                args
            }
            Platform::Android => {
                let ndk = env::var_os("ANDROID_NDK_HOME")
                    .map_or_else(|| home().join(DEFAULT_ANDROID_NDK), PathBuf::from);
                let toolchain = ndk.join("build/cmake/android.toolchain.cmake");
                assert!(
                    toolchain.is_file(),
                    "build.rs: llama-sys android 目标缺 NDK 工具链文件 {}（设 ANDROID_NDK_HOME）",
                    toolchain.display()
                );
                let mut args = cross;
                args.extend([
                    format!("-DCMAKE_TOOLCHAIN_FILE={}", toolchain.display()),
                    "-DANDROID_ABI=arm64-v8a".to_owned(),
                    "-DANDROID_PLATFORM=android-24".to_owned(),
                    "-DANDROID_STL=c++_static".to_owned(),
                    format!("-DGGML_CPU_ARM_ARCH={}", arm_arch()),
                ]);
                args
            }
            Platform::Ohos => {
                let sdk = env::var_os("OHOS_SDK_NATIVE")
                    .map_or_else(|| PathBuf::from(DEFAULT_OHOS_SDK_NATIVE), PathBuf::from);
                let toolchain = sdk.join("build/cmake/ohos.toolchain.cmake");
                assert!(
                    toolchain.is_file(),
                    "build.rs: llama-sys ohos 目标缺 DevEco 工具链文件 {}（设 OHOS_SDK_NATIVE）",
                    toolchain.display()
                );
                let mut args = cross;
                args.extend([
                    format!("-DCMAKE_TOOLCHAIN_FILE={}", toolchain.display()),
                    "-DOHOS_ARCH=arm64-v8a".to_owned(),
                    "-DOHOS_STL=c++_shared".to_owned(),
                    format!("-DGGML_CPU_ARM_ARCH={}", arm_arch()),
                    // 上游 ggml/src/CMakeLists.txt 只在 CMAKE_SYSTEM_NAME 为 Linux / Android 时加 _GNU_SOURCE，鸿蒙工具链报 OHOS；
                    // musl sched.h 的 cpu_set_t / CPU_ZERO 在 _GNU_SOURCE 之后才有，ggml-cpu.c 线程亲和段编不过。
                    // 从命令行补进去，不打 third_party 补丁（上游反馈见报告 §7）。
                    // ohos.toolchain.cmake 给每次编译都加 `--gcc-toolchain=…`，clang 报 unused-command-line-argument；
                    // 桥是 -Werror，这条告警要压掉（只影响诊断，不改码生成）。
                    "-DCMAKE_C_FLAGS=-D_GNU_SOURCE -Wno-unused-command-line-argument".to_owned(),
                    "-DCMAKE_CXX_FLAGS=-D_GNU_SOURCE -Wno-unused-command-line-argument".to_owned(),
                ]);
                args
            }
        }
    }

    /// 预编静态库目录（`MADINFER_LLAMA_LIB_DIR`）在场且 5 个 `.a` 齐全 → 返回它；缺任一即 panic 点名（不静默回退 cmake）。
    fn prebuilt_lib_dir() -> Option<PathBuf> {
        let dir = PathBuf::from(env::var_os("MADINFER_LLAMA_LIB_DIR")?);
        let missing: Vec<String> = STATIC_LIBS
            .iter()
            .map(|lib| format!("lib{lib}.a"))
            .filter(|name| !dir.join(name).is_file())
            .collect();
        assert!(
            missing.is_empty(),
            "build.rs: MADINFER_LLAMA_LIB_DIR={} 缺静态库 {}（tools/llama/build_targets.sh 产出的 <triple>/ 目录才完整）",
            dir.display(),
            missing.join(" ")
        );
        Some(dir)
    }

    /// configure 参数变了就整目录重来：cmake 对已配置目录忽略新的 `CMAKE_TOOLCHAIN_FILE` / `CMAKE_SYSTEM_NAME`，
    /// 只改缓存里能改的项——结果是半棵宿主对象树被交叉工具链续编（本卡鸿蒙首跑即撞上），不如按 stamp 判等重配。
    fn reset_build_dir_if_args_changed(build_dir: &Path, args: &[String]) {
        let stamp = build_dir.join("madinfer-configure-args.txt");
        let wanted = args.join("\n");
        if std::fs::read_to_string(&stamp).ok().as_deref() == Some(wanted.as_str()) {
            return;
        }
        if build_dir.exists() {
            std::fs::remove_dir_all(build_dir)
                .unwrap_or_else(|e| panic!("build.rs: 清理 {} 失败：{e}", build_dir.display()));
        }
        std::fs::create_dir_all(build_dir)
            .unwrap_or_else(|e| panic!("build.rs: 创建 {} 失败：{e}", build_dir.display()));
        std::fs::write(&stamp, wanted)
            .unwrap_or_else(|e| panic!("build.rs: 写 {} 失败：{e}", stamp.display()));
    }

    fn cmake_build(csrc: &Path, build_dir: &Path, platform: &Platform) {
        let cmake = env::var("CMAKE").unwrap_or_else(|_| "cmake".to_owned());
        let args = toolchain_args(platform);
        reset_build_dir_if_args_changed(build_dir, &args);
        let mut configure = Command::new(&cmake);
        configure
            .arg("-S")
            .arg(csrc)
            .arg("-B")
            .arg(build_dir)
            .arg("-DCMAKE_BUILD_TYPE=Release")
            .args(&args);
        run("configure", &mut configure);
        run(
            "build",
            Command::new(&cmake)
                .arg("--build")
                .arg(build_dir)
                .arg("--target")
                .arg(BRIDGE_TARGET)
                .arg("--config")
                .arg("Release")
                .arg("-j")
                .arg(jobs()),
        );
    }

    pub fn build() {
        let manifest_dir = env_path("CARGO_MANIFEST_DIR");
        let csrc = manifest_dir.join("csrc");
        let vendored = manifest_dir.join("../../third_party/llama.cpp");
        let build_dir = env_path("OUT_DIR").join("llama-build");

        println!("cargo:rerun-if-changed={}", csrc.display());
        println!("cargo:rerun-if-changed={}", vendored.display());
        for key in [
            "CMAKE",
            "MADINFER_LLAMA_CMAKE_JOBS",
            "MADINFER_LLAMA_LIB_DIR",
            "MADINFER_LLAMA_ARM_ARCH",
            "ANDROID_NDK_HOME",
            "OHOS_SDK_NATIVE",
        ] {
            println!("cargo:rerun-if-env-changed={key}");
        }

        let platform = platform();
        let lib_dir = match prebuilt_lib_dir() {
            Some(prebuilt) => {
                println!(
                    "cargo:warning=llama-sys: 用预编静态库 {}（跳过 cmake）",
                    prebuilt.display()
                );
                prebuilt
            }
            None => {
                cmake_build(&csrc, &build_dir, &platform);
                build_dir.join("lib")
            }
        };

        println!("cargo:rustc-link-search=native={}", lib_dir.display());
        for lib in STATIC_LIBS {
            println!("cargo:rustc-link-lib=static={lib}");
        }
        // C++ 运行时：ggml / llama 是 C++17。Apple 平台系统 libc++；Android 随 ANDROID_STL=c++_static 静态收纳
        // libc++ + libc++abi（NDK sysroot 内 .a，链接器 = NDK clang）；OHOS 用 DevEco 的 libc++（c++_shared，
        // 与 mad 侧 libmadaikit.so 的 DT_NEEDED libc++_shared.so 同一份）；其余 libstdc++。
        match platform {
            Platform::Macos | Platform::Ios { .. } => println!("cargo:rustc-link-lib=c++"),
            // 不写 `static=`：那会让 rustc 在本 crate 的 rlib 阶段就去找归档并打包进 rlib，而 NDK sysroot 不在 link-search；
            // 交给最终链接的 NDK clang 以 -lc++_static -lc++abi 从 sysroot 取。
            Platform::Android => {
                println!("cargo:rustc-link-lib=c++_static");
                println!("cargo:rustc-link-lib=c++abi");
            }
            Platform::Ohos => println!("cargo:rustc-link-lib=c++"),
            Platform::Other => println!("cargo:rustc-link-lib=stdc++"),
        }
    }
}
