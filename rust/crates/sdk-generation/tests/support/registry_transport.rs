fn main() {
    #[cfg(windows)]
    {
        #[link(name = "kernel32")]
        unsafe extern "system" {
            #[link_name = "GetConsoleWindow"]
            fn console_window() -> *mut std::ffi::c_void;
        }
        // SAFETY: this query takes no arguments and returns an OS-owned handle.
        assert!(
            unsafe { console_window() }.is_null(),
            "registry transport acquired a console"
        );
    }
    let args = std::env::args().collect::<Vec<_>>();
    assert!(args.windows(2).any(|pair| pair[0] == "--user-agent"
        && pair[1].starts_with("acyclic-sdk-docs/")
        && pair[1].contains("https://github.com/acyclic-labs/sdk")));
    let records = std::fs::read_to_string(
        std::env::current_exe()
            .unwrap()
            .parent()
            .unwrap()
            .join("registry-responses.txt"),
    )
    .unwrap();
    let lines = records.lines().collect::<Vec<_>>();
    for pair in lines.chunks_exact(2) {
        if args.last().unwrap().ends_with(pair[0]) {
            print!("{}\n200", pair[1]);
            return;
        }
    }
    print!("\n404");
}
