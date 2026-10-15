//! Select modeled primitives only in the verification crate.
fn main() {
    // Instrument only this verification crate; dependencies keep their native
    // configuration (in particular Tokio's production filesystem APIs).
    println!("cargo:rustc-check-cfg=cfg(loom)");
    println!("cargo:rustc-cfg=loom");
}
