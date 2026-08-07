use std::{env, path::PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=vendor/espeak-ng/src");
    println!("cargo:rerun-if-changed=vendor/espeak-ng/cmake");
    println!("cargo:rerun-if-changed=vendor/espeak-ng/CMakeLists.txt");

    let source = PathBuf::from(
        env::var_os("CARGO_MANIFEST_DIR").expect("Cargo did not provide CARGO_MANIFEST_DIR"),
    )
    .join("vendor/espeak-ng");
    assert!(
        source.join("CMakeLists.txt").is_file(),
        "vendor/espeak-ng is missing; clone with --recurse-submodules"
    );
    let mut native = cmake::Config::new(&source);
    native
        .define("BUILD_SHARED_LIBS", "OFF")
        .define("BUILD_TESTING", "OFF")
        .define("ESPEAK_BUILD_MANPAGES", "OFF")
        .define("USE_ASYNC", "OFF")
        .define("USE_MBROLA", "OFF")
        .define("USE_LIBSONIC", "OFF")
        .define("USE_LIBPCAUDIO", "OFF")
        // eSpeak NG 1.52 probes/fetches sonic even when support is disabled.
        // Supplying inert cache values keeps builds offline; neither is linked.
        .define("SONIC_LIB", "unused")
        .define("SONIC_INC", &source)
        .build_target("espeak-ng");
    if env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        native.cflag("/utf-8").cxxflag("/utf-8");
    }
    let destination = native.build();

    let build = destination.join("build");
    for directory in [
        build.join("src/libespeak-ng"),
        build.join("src/libespeak-ng/Debug"),
        build.join("src/libespeak-ng/Release"),
        build.join("src/speechPlayer"),
        build.join("src/speechPlayer/Debug"),
        build.join("src/speechPlayer/Release"),
        build.join("src/ucd-tools"),
        build.join("src/ucd-tools/Debug"),
        build.join("src/ucd-tools/Release"),
    ] {
        println!("cargo:rustc-link-search=native={}", directory.display());
    }
    println!("cargo:rustc-link-lib=static=espeak-ng");
    println!("cargo:rustc-link-lib=static=speechPlayer");
    println!("cargo:rustc-link-lib=static=ucd");

    match env::var("CARGO_CFG_TARGET_OS").as_deref() {
        Ok("macos") => println!("cargo:rustc-link-lib=c++"),
        Ok("linux" | "freebsd" | "openbsd" | "netbsd" | "dragonfly") => {
            println!("cargo:rustc-link-lib=stdc++");
            println!("cargo:rustc-link-lib=m");
        }
        _ => {}
    }
}
