//! Embeds `manifest.xml` into the `logscope-setup` executable.
//!
//! The manifest declares the comctl32 v6 dependency that rfd's
//! `common-controls-v6` feature requires. Opting into that feature without
//! embedding a manifest produces a binary that links fine and then dies at
//! load time with STATUS_ENTRYPOINT_NOT_FOUND, because `TaskDialogIndirect`
//! only exists in comctl32 v6 and v6 is only bound when a manifest asks for
//! it. That is exactly how the first packaged setup executable failed.
//!
//! MSVC-only by design: the only public desktop artifact is Windows x64
//! MSVC (ADR-0002), and scoping the linker args keeps every other target -
//! including the Linux and macOS shared-core CI legs - completely
//! untouched. The `append_payload` helper bin is likewise excluded via the
//! `-bin=logscope-setup` scoping: a console tool needs no comctl32.

fn main() {
    println!("cargo:rerun-if-changed=manifest.xml");

    let os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let env = std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    if os != "windows" || env != "msvc" {
        return;
    }

    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("manifest.xml");
    println!("cargo:rustc-link-arg-bin=logscope-setup=/MANIFEST:EMBED");
    println!(
        "cargo:rustc-link-arg-bin=logscope-setup=/MANIFESTINPUT:{}",
        manifest.display()
    );
}
