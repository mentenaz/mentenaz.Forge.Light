use std::path::PathBuf;

fn main() {
    // Dynamically grab the crate directory to avoid workspace resolution errors
    let mut slint_path = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    slint_path.push("ui");
    slint_path.push("app.slint");

    // Tell Cargo to re-run this script if the .slint file changes
    println!("cargo:rerun-if-changed={}", slint_path.display());

    // Compile it
    slint_build::compile(slint_path.to_str().unwrap()).unwrap();
}
