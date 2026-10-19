//! Embeds a Windows application manifest in canopy.exe. On other platforms
//! this does nothing.
//!
//! The manifest says what Windows expects a current program to say: that it's
//! aware of each monitor's DPI from the start (winit also asks at run time,
//! but only once its event loop exists), that it wants version 6 of the
//! common controls (themed message boxes, and the native controls inside
//! hosted plugins' editors), that it handles long paths, and which Windows
//! versions it knows, so none of them runs it in a compatibility mode.

use std::env;

use embed_manifest::manifest::ActiveCodePage;
use embed_manifest::{embed_manifest, new_manifest};

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    if env::var_os("CARGO_CFG_WINDOWS").is_none() {
        return;
    }
    let manifest = new_manifest("canopy")
        // Rust calls Windows' UTF-16 functions either way; leave the code page
        // the user chose to the code that doesn't, such as hosted plugins.
        .active_code_page(ActiveCodePage::System);

    // For MSVC targets embed-manifest has the linker merge the manifest in
    // (/MANIFESTINPUT). Microsoft's link.exe can, but cross builds link with
    // LLVM's lld-link, which can only if it was built with libxml2, and
    // rustup's rust-lld wasn't ("unable to find mt.exe"). So when
    // cross-compiling, take embed-manifest's MinGW route instead: a COFF
    // object holding the manifest as a resource, which any linker takes.
    let target = env::var("TARGET").unwrap_or_default();
    let host = env::var("HOST").unwrap_or_default();
    if target.ends_with("-windows-msvc") && !host.contains("-windows-") {
        let as_mingw = target.replace("-windows-msvc", "-windows-gnu");
        // SAFETY: a build script is one thread; nothing reads the environment
        // concurrently.
        unsafe { env::set_var("TARGET", as_mingw) };
        // The resource is the manifest; the linker mustn't add one of its own.
        println!("cargo:rustc-link-arg-bins=/MANIFEST:NO");
    }
    embed_manifest(manifest).expect("couldn't embed the Windows manifest");
}
