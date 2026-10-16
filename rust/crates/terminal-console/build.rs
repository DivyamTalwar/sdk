//! Build the Windows console ABI shim only on Windows hosts.
fn main() {
    #[cfg(windows)]
    {
        cxx_build::bridge("src/lib.rs")
            .file("src/windows.cpp")
            .include(".")
            .std("c++17")
            .flag_if_supported("/EHsc")
            .compile("acyclic-terminal-console");
        println!("cargo::rerun-if-changed=src/lib.rs");
        println!("cargo::rerun-if-changed=src/windows.cpp");
        println!("cargo::rerun-if-changed=src/windows.h");
    }
}
