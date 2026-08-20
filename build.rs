use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=assets/resources.gresource.xml");
    println!("cargo:rerun-if-changed=assets/signal-chat-export-icon.svg");

    let output = PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR must be set"))
        .join("signal-filter.gresource");
    let status = Command::new("glib-compile-resources")
        .arg("--target")
        .arg(output)
        .arg("--sourcedir=assets")
        .arg("assets/resources.gresource.xml")
        .status()
        .expect("glib-compile-resources is required");
    assert!(status.success(), "failed to compile GTK resources");
}
