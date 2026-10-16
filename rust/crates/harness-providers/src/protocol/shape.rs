//! Required wire fields, shared by local validators and native replay checks.
use super::name::{CALL_ID, Name};
use serde_json::{Value, value::RawValue};

#[derive(serde::Deserialize)]
struct Text<'a>(#[serde(borrow)] std::borrow::Cow<'a, str>);

#[derive(Clone, Copy)]
pub(crate) enum Constraint {
    String { nonempty: bool },
    Object,
    Array,
    Name(Name),
    ObjectJson,
    AnyString(&'static [&'static str]),
}
pub(crate) struct Shape {
    pub discriminator: (&'static str, &'static str),
    pub fields: &'static [(&'static str, Constraint)],
}
impl Constraint {
    pub fn accepts(self, value: &Value) -> bool {
        match self {
            Self::String { nonempty } => value
                .as_str()
                .is_some_and(|text| !nonempty || !text.is_empty()),
            Self::Object => value.is_object(),
            Self::Array => value.is_array(),
            Self::Name(rule) => value.as_str().is_some_and(|text| rule.accepts(text)),
            Self::ObjectJson => value
                .as_str()
                .and_then(|text| serde_json::from_str::<Value>(text).ok())
                .is_some_and(|value| value.is_object()),
            Self::AnyString(paths) => paths.iter().any(|path| {
                value
                    .pointer(path)
                    .is_some_and(|value| NONEMPTY.accepts(value))
            }),
        }
    }
    pub fn accepts_raw(self, value: Option<&RawValue>) -> bool {
        let Some(value) = value else {
            return false;
        };
        match self {
            Self::String { nonempty } => serde_json::from_str::<Text<'_>>(value.get())
                .ok()
                .is_some_and(|text| !nonempty || !text.0.is_empty()),
            Self::Array => value.get().starts_with('['),
            _ => false,
        }
    }
}
impl Shape {
    pub fn accepts(&self, value: &Value) -> bool {
        value.is_object()
            && value[self.discriminator.0] == self.discriminator.1
            && self.fields.iter().all(|(path, constraint)| {
                value
                    .pointer(path)
                    .is_some_and(|value| constraint.accepts(value))
            })
    }
}
const STRING: Constraint = Constraint::String { nonempty: false };
const NONEMPTY: Constraint = Constraint::String { nonempty: true };
const FUNCTION_NAME: Constraint = Constraint::Name(Name {
    maximum: 64,
    leading_letter: false,
    punctuation: b"_-",
});
const TEXT_FIELDS: &[(&str, Constraint)] = &[("/text", STRING)];
pub(crate) const CONTENT: &[Shape] = &[
    Shape {
        discriminator: ("type", "text"),
        fields: TEXT_FIELDS,
    },
    Shape {
        discriminator: ("type", "input_text"),
        fields: TEXT_FIELDS,
    },
    Shape {
        discriminator: ("type", "output_text"),
        fields: TEXT_FIELDS,
    },
    Shape {
        discriminator: ("type", "refusal"),
        fields: &[("/refusal", STRING)],
    },
    Shape {
        discriminator: ("type", "image_url"),
        fields: &[("/image_url/url", NONEMPTY)],
    },
    Shape {
        discriminator: ("type", "input_image"),
        fields: &[("", Constraint::AnyString(&["/image_url", "/file_id"]))],
    },
    Shape {
        discriminator: ("type", "input_audio"),
        fields: &[
            ("/input_audio/data", NONEMPTY),
            ("/input_audio/format", NONEMPTY),
        ],
    },
    Shape {
        discriminator: ("type", "file"),
        fields: &[("/file", Constraint::Object)],
    },
    Shape {
        discriminator: ("type", "input_file"),
        fields: &[(
            "",
            Constraint::AnyString(&["/file_id", "/file_url", "/file_data"]),
        )],
    },
];
pub(crate) fn content(value: &Value) -> bool {
    CONTENT.iter().any(|shape| shape.accepts(value))
}
pub(crate) const CHAT_CALL: Shape = Shape {
    discriminator: ("type", "function"),
    fields: &[
        ("/id", Constraint::Name(CALL_ID)),
        ("/function/name", FUNCTION_NAME),
        ("/function/arguments", Constraint::ObjectJson),
    ],
};
pub(crate) const RESPONSE_CALL: Shape = Shape {
    discriminator: ("type", "function_call"),
    fields: &[
        (
            "/call_id",
            Constraint::Name(Name {
                maximum: 64,
                ..CALL_ID
            }),
        ),
        ("/name", FUNCTION_NAME),
        ("/arguments", Constraint::ObjectJson),
    ],
};
pub(crate) const THINKING: Shape = Shape {
    discriminator: ("type", "thinking"),
    fields: &[("/thinking", STRING), ("/signature", NONEMPTY)],
};
pub(crate) const REDACTED: Shape = Shape {
    discriminator: ("type", "redacted_thinking"),
    fields: &[("/data", NONEMPTY)],
};
pub(crate) const REASONING: Shape = Shape {
    discriminator: ("type", "reasoning"),
    fields: &[
        ("/id", NONEMPTY),
        ("/summary", Constraint::Array),
        ("/encrypted_content", NONEMPTY),
    ],
};
