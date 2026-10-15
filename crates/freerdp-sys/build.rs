fn main() {
    println!("cargo:rerun-if-changed=shim/ferrum_shim.c");
    println!("cargo:rerun-if-changed=shim/ferrum_shim.h");
    println!("cargo:rerun-if-env-changed=PKG_CONFIG_PATH");

    let mut build = cc::Build::new();
    build
        .file("shim/ferrum_shim.c")
        .std("gnu2x")
        .warnings(true)
        .flag_if_supported("-Wno-deprecated-declarations")
        // Distro CFLAGS may enable GCC LTO; rust-lld cannot read GIMPLE objects.
        .flag_if_supported("-fno-lto");

    // MinGW has no <threads.h> but does not say so; winpr's platform.h
    // would include it in C11 mode.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        build.define("__STDC_NO_THREADS__", "1");
    }

    for lib in ["freerdp3", "freerdp-client3", "winpr3"] {
        let lib = pkg_config::Config::new()
            .atleast_version("3.10")
            .probe(lib)
            .unwrap_or_else(|e| {
                panic!("{lib} not found ({e}). Install FreeRDP 3: `sudo pacman -S freerdp`")
            });
        for path in lib.include_paths {
            build.include(path);
        }
    }

    build.compile("ferrum_shim");
}
