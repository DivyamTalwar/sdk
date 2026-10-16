use serde::{Deserialize, Serialize};

/// Provider-neutral reasoning retained in the canonical transcript.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "wasm", derive(tsify::Tsify))]
pub struct ReasoningBlock {
    /// Meaning of the retained content, independent of its wire representation.
    pub kind: ReasoningKind,
    /// Readable content verified by the origin adapter, when available.
    pub text: Option<String>,
    /// Provider family, model and credential ownership of the native payload.
    pub origin: ReasoningOrigin,
    /// Exact native continuation bytes; projection never edits this payload.
    pub continuity: Vec<u8>,
    /// What the origin adapter could preserve.
    pub outcome: ReasoningOutcome,
}

/// Origin identity used when choosing native replay or foreign context.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "wasm", derive(tsify::Tsify))]
pub struct ReasoningOrigin {
    /// Protocol family, rather than a display name or registry alias.
    pub provider: String,
    /// Exact selected model that produced this block.
    pub model: String,
    /// Opaque nonsecret credential-scope identity; empty means unknown.
    pub credential_scope: String,
}

/// Reasoning shapes normalized by the adapters.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "wasm", derive(tsify::Tsify))]
pub enum ReasoningKind {
    /// Plain reasoning text.
    Readable,
    /// A provider's readable summary.
    Summary,
    /// Encrypted continuation state.
    Encrypted,
    /// Redacted continuation state.
    Redacted,
    /// An opaque signature, including one attached to a tool call.
    Signature,
    /// Another native continuation shape.
    Opaque,
}

/// Preservation result recorded without changing the original transcript block.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[cfg_attr(feature = "wasm", derive(tsify::Tsify))]
pub enum ReasoningOutcome {
    /// Readable content was preserved.
    PreservedReadable,
    /// Native opaque continuity was preserved.
    PreservedOpaque,
    /// Neither readable nor native content was available.
    MissingContent,
    /// Display was intentionally suppressed while continuity was retained.
    SuppressedDisplay,
    /// The target cannot accept this native shape.
    UnsupportedShape,
}
