#![no_main]

use libfuzzer_sys::fuzz_target;

#[path = "../../src/client/commands/syntax.rs"]
mod syntax;

#[derive(Debug)]
pub enum Error {
    Invalid(String),
}
pub type Result<T> = std::result::Result<T, Error>;

fuzz_target!(|input: &str| {
    let maximum = input.len() as u64;
    let result = syntax::split(input, maximum);
    if input.is_empty() {
        assert!(result.is_err());
    } else if let Some((name, arguments)) = result.expect("positive exact input bound") {
        let normalized = format!("/{name} {arguments}");
        assert_eq!(
            syntax::split(&normalized, normalized.len() as u64).expect("normalized"),
            Some((name, arguments))
        );
    } else {
        assert!(!input.trim_start().starts_with('/'));
    }
    assert!(syntax::split(input, 0).is_err());
    if !input.is_empty() {
        assert!(syntax::split(input, maximum - 1).is_err());
    }
});
