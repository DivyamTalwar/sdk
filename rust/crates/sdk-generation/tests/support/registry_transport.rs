mod compiler_probe;

fn main() {
    let arguments = std::env::args_os().skip(1).collect::<Vec<_>>();
    if arguments
        .first()
        .is_some_and(|arg| std::path::Path::new(arg).is_absolute())
    {
        match compiler_probe::run(&arguments) {
            Ok(code) => std::process::exit(code),
            Err(error) => {
                eprintln!("fixture compiler: {error}");
                std::process::exit(1);
            }
        }
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
