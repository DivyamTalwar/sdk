pub(crate) fn json_line(stdout: &str) -> Option<&str> {
    stdout.lines().find(|line| line.starts_with('{'))
}

#[cfg(test)]
proptest::proptest! {
    #[test]
    fn cargo_output_keeps_the_complete_json_record(
        messages in proptest::collection::vec(".{0,24}", 0..8),
        values in proptest::collection::btree_map(".{0,12}", ".{0,24}", 0..8),
    ) {
        let json = serde_json::to_string(&values).unwrap();
        let output = messages.iter().map(|message| format!("warning: {message}\n"))
            .collect::<String>() + &json + "\ntrailing message\n";
        let record = json_line(&output).unwrap();
        proptest::prop_assert_eq!(record, json.as_str());
        proptest::prop_assert_eq!(serde_json::from_str::<std::collections::BTreeMap<String,String>>(record).unwrap(), values);
        proptest::prop_assert!(json_line("no metadata\n").is_none());
    }
}
