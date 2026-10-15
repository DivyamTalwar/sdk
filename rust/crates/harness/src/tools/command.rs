use crate::{Error, Result};

/// Splits a facade command into arguments without executing shell expansion.
pub fn parse_command(command: &str) -> Result<Vec<String>> {
    let mut arguments = Vec::new();
    let mut word = String::new();
    let mut quote = None;
    let mut escaped = false;
    let mut started = false;
    for character in command.chars() {
        if character == '\0' {
            return Err(Error::Invalid("fs command contains NUL".into()));
        }
        if escaped {
            word.push(character);
            escaped = false;
        } else if character == '\\' && quote != Some('\'') {
            escaped = true;
            started = true;
        } else if let Some(delimiter) = quote {
            if character == delimiter {
                quote = None;
            } else {
                word.push(character);
            }
        } else if matches!(character, '\'' | '"') {
            quote = Some(character);
            started = true;
        } else if character.is_whitespace() {
            if started {
                arguments.push(std::mem::take(&mut word));
                started = false;
            }
        } else if matches!(character, ';' | '|' | '&' | '<' | '>' | '`') {
            return Err(Error::Invalid("fs accepts one facade command".into()));
        } else {
            word.push(character);
            started = true;
        }
    }
    if escaped || quote.is_some() {
        return Err(Error::Invalid("unterminated fs command argument".into()));
    }
    if started {
        arguments.push(word);
    }
    if arguments.is_empty() {
        return Err(Error::Invalid("fs command is empty".into()));
    }
    Ok(arguments)
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod properties {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        // Property: single-quoted arguments round trip without shell expansion.
        #[test]
        fn quoted_arguments_round_trip(words in prop::collection::vec("[^\u{0}']{0,24}", 1..8)) {
            let command = words.iter().map(|word| format!("'{word}'")).collect::<Vec<_>>().join(" ");
            prop_assert_eq!(parse_command(&command)?, words);
        }

        #[test]
        fn nul_is_rejected(prefix in ".{0,32}", suffix in ".{0,32}") {
            let command = format!("{prefix}\0{suffix}");
            prop_assert!(parse_command(&command).is_err());
        }
    }
}
