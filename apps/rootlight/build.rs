//! Reserve the MSVC CLI stack required by synchronous indexing.

fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows")
        && std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc")
    {
        // Synchronous indexing exceeds MSVC's 1 MiB default in debug builds.
        // Reserve address space while keeping stack pages committed on demand.
        println!("cargo:rustc-link-arg-bin=rootlight=/STACK:2097152");
    }
}
