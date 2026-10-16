#![no_main]

use acyclic_harness::{
    Error, OperationId, Result,
    client::{CommandCatalog, CommandFileChange, CommandPaletteState, CommandResolution},
    conversation::ContentResidencyVerifier,
};
use libfuzzer_sys::fuzz_target;
use std::collections::BTreeMap;

#[path = "../../src/client/commands/syntax.rs"]
mod syntax;
struct NoReads;
impl ContentResidencyVerifier for NoReads {}

fuzz_target!(|input: &[u8]| {
    if let Ok(catalog) = CommandCatalog::from_bytes(input, 8192) {
        let specs: Vec<_> = catalog.commands().collect();
        let encoded = serde_json::to_vec(&specs).unwrap();
        let round_trip = CommandCatalog::from_bytes(&encoded, encoded.len() as u64).unwrap();
        assert_eq!(round_trip.commands().collect::<Vec<_>>(), specs);
    }
    let catalog = CommandCatalog::builtins().unwrap();
    if let Ok(state) = serde_json::from_slice::<CommandPaletteState>(input) {
        let palette = catalog.palette(&state);
        for state in [palette.next(), palette.previous()] {
            assert!(state.selected < palette.entries().len().max(1) as u64);
            let encoded = serde_json::to_vec(&state).unwrap();
            assert_eq!(
                serde_json::from_slice::<CommandPaletteState>(&encoded).unwrap(),
                state
            );
        }
    }
    let snapshot: BTreeMap<String, serde_json::Value> =
        serde_json::from_slice(input).unwrap_or_default();
    if let Ok(resolution) = serde_json::from_slice::<CommandResolution>(input) {
        if let Ok(changes) = resolution.file_changes(&snapshot, OperationId::new()) {
            assert!(changes.len() <= 1);
            let encoded = serde_json::to_vec(&changes).unwrap();
            assert_eq!(
                serde_json::from_slice::<Vec<CommandFileChange>>(&encoded).unwrap(),
                changes
            );
        }
    }
    let Ok(input) = std::str::from_utf8(input) else {
        return;
    };
    let maximum = input.len() as u64;
    let result = syntax::split(input, maximum);
    if input.is_empty() {
        assert!(result.is_err());
    } else if let Some((name, arguments)) = result.expect("positive exact input bound") {
        let normalized = format!("/{name} {arguments}");
        assert_eq!(
            syntax::split(&normalized, normalized.len() as u64).unwrap(),
            Some((name, arguments))
        );
    } else {
        assert!(!input.trim_start().starts_with('/'));
    }
    assert!(syntax::split(input, 0).is_err());
    if !input.is_empty() {
        assert!(syntax::split(input, maximum - 1).is_err());
    }
    let index =
        input.as_bytes().first().copied().unwrap_or_default() as usize % catalog.commands().count();
    let name = &catalog.commands().nth(index).unwrap().name;
    let command = format!("/{name} {input}");
    if let Ok(resolution) = futures::executor::block_on(catalog.resolve(&command, &NoReads, 8192)) {
        assert!(!matches!(&resolution, CommandResolution::Prompt { .. }));
        if let Ok(changes) = resolution.file_changes(&snapshot, OperationId::new()) {
            assert!(changes.len() <= 1);
        }
    }
});
