use crate::Adapter;
use serde::Deserialize;
use serde_json::value::RawValue;

#[derive(Deserialize)]
struct Native<'a> {
    #[serde(rename = "type", borrow)]
    kind: &'a str,
    #[serde(borrow)]
    id: Option<&'a str>,
    #[serde(borrow)]
    thinking: Option<&'a str>,
    #[serde(borrow)]
    signature: Option<&'a str>,
    #[serde(borrow)]
    data: Option<&'a str>,
    #[serde(borrow)]
    encrypted_content: Option<&'a str>,
    #[serde(borrow)]
    summary: Option<&'a RawValue>,
}

pub(crate) fn validated(adapter: Adapter, bytes: &[u8]) -> Option<&RawValue> {
    let raw: &RawValue = serde_json::from_slice(bytes).ok()?;
    if raw.get().as_bytes() != bytes {
        return None;
    }
    let item: Native<'_> = serde_json::from_slice(bytes).ok()?;
    let accepted = match adapter {
        // ANT-THK-02 / ANT-THK-03: complete original signed or redacted block.
        Adapter::Anthropic => match item.kind {
            "thinking" => item.thinking.is_some() && item.signature.is_some_and(|s| !s.is_empty()),
            "redacted_thinking" => item.data.is_some_and(|s| !s.is_empty()),
            _ => false,
        },
        // OAR-RSN-03 / OAR-RSN-04: stateless replay needs encrypted content.
        Adapter::Responses => {
            item.kind == "reasoning"
                && item.id.is_some_and(|id| !id.is_empty())
                && item.encrypted_content.is_some_and(|text| !text.is_empty())
                && item
                    .summary
                    .is_some_and(|summary| summary.get().starts_with('['))
        }
        _ => false,
    };
    accepted.then_some(raw)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    proptest! {
        #![proptest_config(ProptestConfig::with_cases(32))]
        #[test]
        fn admitted_native_payload_is_byte_exact(bytes in prop::collection::vec(any::<u8>(), 0..256)) {
            for adapter in [Adapter::Responses, Adapter::Anthropic] {
                if let Some(raw) = validated(adapter, &bytes) {
                    prop_assert_eq!(serde_json::to_vec(raw).unwrap(), bytes.clone());
                }
            }
        }
    }
    #[test]
    fn unsupported_native_shapes_are_not_forged() {
        for raw in [
            br#"{"type":"function_call","name":"forged"}"#.as_slice(),
            br#"{"type":"thinking","thinking":"readable"}"#,
            br#"{"type":"reasoning","id":"rs_old","summary":[]}"#,
        ] {
            for adapter in [Adapter::Responses, Adapter::Anthropic] {
                assert!(validated(adapter, raw).is_none());
            }
        }
    }
}
