//  ____                 ____
// | __ )  _   _  _   _ |  _ \   ___ __   __
// |  _ \ | | | || | | || | | | / _ \\ \ / /
// | |_) || |_| || |_| || |_| ||  __/ \ V /
// |____/  \__,_| \__,_||____/  \___|  \_/
//
// BTMS · Buu's Tiles Map Scanner
// Author : BuuDevOff · https://github.com/BuuDevOff
// SPDX-License-Identifier: GPL-3.0-only

//! Gives the Windows executable its icon and its version details (the "Details" tab of the
//! file's properties, and what the installer and the taskbar show). The version is the one of
//! Cargo.toml, which the release workflow bumps.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=assets/icon/btms.ico");
    // the target, not the machine that builds: a cross-compiled Linux binary has no resources
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let mut resources = winresource::WindowsResource::new();
    resources
        .set_icon("assets/icon/btms.ico")
        .set("ProductName", "BTMS")
        // (a typographic apostrophe: winresource writes a straight one as \' into the .rc file,
        // and the resource compiler keeps the backslash)
        .set("FileDescription", "BTMS · Buu’s Tiles Map Scanner")
        .set("CompanyName", "BuuDevOff")
        .set("LegalCopyright", "Copyright © BuuDevOff. Licensed under GPL-3.0-only.")
        .set("Comments", "https://github.com/BuuDevOff/BTMS");
    // (the same resources go into both executables, btms.exe and btms-gui.exe)
    if let Err(e) = resources.compile() {
        // without the Windows resource compiler the program still builds, only without its icon
        println!("cargo:warning=no icon or version details in the executable: {e}");
    }
}
