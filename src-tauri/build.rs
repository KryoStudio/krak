fn main() {
    // NVENC shim (Windows only): a small C layer compiled against NVIDIA's vendored
    // nvEncodeAPI.h, so no NVENC structure is ever re-declared by hand in Rust.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        println!("cargo:rerun-if-changed=native/kryo_nvenc.c");
        println!("cargo:rerun-if-changed=native/kryo_nvenc.h");
        println!("cargo:rerun-if-changed=native/nvenc/nvEncodeAPI.h");
        cc::Build::new()
            .file("native/kryo_nvenc.c")
            .include("native")
            .warnings(true)
            .warnings_into_errors(true)
            .compile("kryo_nvenc");
    }
    tauri_build::build()
}
