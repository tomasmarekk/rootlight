//! Compile-time provenance for the offline benchmark executable.

use std::{env, error::Error, io, process::Command};

fn main() -> Result<(), Box<dyn Error>> {
    println!("cargo:rerun-if-env-changed=RUSTC");
    println!("cargo:rerun-if-env-changed=TARGET");
    println!("cargo:rerun-if-env-changed=PROFILE");

    let rustc = env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
    let output = Command::new(rustc).arg("-V").output()?;
    if !output.status.success() {
        return Err(io::Error::other("rustc identity command failed").into());
    }
    let identity = String::from_utf8(output.stdout)?;
    let identity = identity.trim();
    if identity.is_empty() || identity.contains(['\r', '\n']) {
        return Err(io::Error::other("rustc identity was not one nonempty line").into());
    }
    let target = env::var("TARGET")?;
    let profile = env::var("PROFILE")?;
    println!("cargo:rustc-env=ROOTLIGHT_BENCH_RUSTC={identity}");
    println!("cargo:rustc-env=ROOTLIGHT_BENCH_TARGET={target}");
    println!("cargo:rustc-env=ROOTLIGHT_BENCH_PROFILE={profile}");
    Ok(())
}
