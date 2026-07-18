//! Compile-time provenance for the offline benchmark executable.

#![forbid(unsafe_code)]

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
    let mut identity_parts = identity.split_ascii_whitespace();
    if identity_parts.next() != Some("rustc") {
        return Err(io::Error::other("rustc identity had an unexpected product name").into());
    }
    let release = identity_parts
        .next()
        .ok_or_else(|| io::Error::other("rustc identity omitted its release"))?;
    let mut components = release.split('.');
    for _ in 0..3 {
        let component = components
            .next()
            .ok_or_else(|| io::Error::other("rustc release omitted a numeric component"))?;
        if component.is_empty()
            || !component.bytes().all(|byte| byte.is_ascii_digit())
            || (component.len() > 1 && component.starts_with('0'))
        {
            return Err(io::Error::other("rustc release was not a source-free token").into());
        }
    }
    if components.next().is_some() {
        return Err(io::Error::other("rustc release had an unexpected suffix").into());
    }
    let identity = format!("rustc-{release}");
    let target = env::var("TARGET")?;
    let profile = env::var("PROFILE")?;
    println!("cargo:rustc-env=ROOTLIGHT_BENCH_RUSTC={identity}");
    println!("cargo:rustc-env=ROOTLIGHT_BENCH_TARGET={target}");
    println!("cargo:rustc-env=ROOTLIGHT_BENCH_PROFILE={profile}");
    Ok(())
}
