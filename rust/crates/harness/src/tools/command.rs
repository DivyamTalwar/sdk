use crate::{Error, Result};

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct FsCommand {
    pub(crate) arguments: Vec<String>,
    pub(crate) volume: Option<String>,
}

pub(crate) fn parse_fs_command(command: &str) -> Result<FsCommand> {
    let mut arguments = parse_command(command)?;
    if arguments.first().is_some_and(|argument| argument == "git") {
        arguments.remove(0);
    }
    let mut volume = None;
    let mut index = 0;
    while index < arguments.len() && arguments[index] != "--" {
        if arguments[index] != "--volume" {
            index += 1;
            continue;
        }
        if volume.is_some()
            || arguments
                .get(index + 1)
                .is_none_or(|name| name.is_empty() || name.starts_with('-'))
        {
            return Err(Error::Invalid(
                "fs requires one nonempty --volume qualifier".into(),
            ));
        }
        let mut qualifier = arguments.drain(index..index + 2);
        qualifier.next();
        volume = qualifier.next();
    }
    if arguments.is_empty() {
        return Err(Error::Invalid("fs command is empty".into()));
    }
    Ok(FsCommand { arguments, volume })
}

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
mod tests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(64))]
        // Quoting is a lossless codec for every non-NUL Unicode argument,
        // including empty strings, metacharacters and whitespace.
        #[test]
        fn quoted_arguments_round_trip(
            arguments in prop::collection::vec(
                prop::collection::vec(any::<char>().prop_filter("non-NUL", |c| *c != '\0'), 0..32)
                    .prop_map(|characters| characters.into_iter().collect::<String>()),
                1..8,
            ),
        ) {
            let command = arguments.iter().map(|argument| {
                format!("\"{}\"", argument.replace('\\', "\\\\").replace('"', "\\\""))
            }).collect::<Vec<_>>().join(" ");
            prop_assert_eq!(parse_command(&command).unwrap(), arguments);
        }

        #[test]
        fn nul_is_always_rejected(prefix in ".{0,64}", suffix in ".{0,64}") {
            let command = format!("{prefix}\0{suffix}");
            prop_assert!(parse_command(&command).is_err());
        }

        // Removing the routing qualifier preserves every facade argument,
        // including operands named --volume after the path delimiter.
        #[test]
        fn volume_qualifier_round_trips(
            name in "[a-z][a-z0-9_-]{0,20}",
            operand in "[a-zA-Z0-9 _-]{0,32}",
            prefix in any::<bool>(),
        ) {
            let command = format!("{}status --volume '{}' -- '{}' --volume", if prefix { "git " } else { "" }, name, operand);
            let parsed = parse_fs_command(&command).unwrap();
            prop_assert_eq!(parsed.volume.as_deref(), Some(name.as_str()));
            prop_assert_eq!(parsed.arguments, ["status", "--", &operand, "--volume"]);
        }
    }

    #[test]
    fn volume_qualifier_requires_a_command_and_one_value() {
        for command in [
            "git",
            "--volume data",
            "status --volume",
            "status --volume ''",
            "status --volume a --volume b",
            "status --volume --",
        ] {
            assert!(parse_fs_command(command).is_err(), "{command}");
        }
    }
}
