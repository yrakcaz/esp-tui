fn main() {
    let manifest = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let target = std::env::var("TARGET").unwrap();
    println!("cargo:rustc-link-search={manifest}/../../target/{target}/release");
    println!("cargo:rustc-link-lib=static=esp_agent");
    println!("cargo:rustc-link-arg=-Wl,--undefined=_esp_agent_ctor");
    embuild::espidf::sysenv::output();
}
