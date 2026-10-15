//! Canonical harness protocol messages and transport bindings.
#![deny(unsafe_code)]

/// Generated Protobuf packages, nested as their package names are, so the harness messages resolve the shared protocol handshake they import.
#[allow(
    missing_docs,
    clippy::pedantic,
    clippy::large_enum_variant,
    reason = "generated prost bindings"
)]
mod generated {
    pub mod acyclic {
        pub mod harness {
            pub mod v2 {
                include!(concat!(env!("OUT_DIR"), "/acyclic.harness.v2.rs"));
            }
        }
        pub mod protocol {
            pub mod v1 {
                include!(concat!(env!("OUT_DIR"), "/acyclic.protocol.v1.rs"));
            }
        }
    }
}

/// Generated Protobuf envelopes shared by every transport: the harness contract and the protocol handshake it negotiates with.
pub mod wire {
    pub use super::generated::acyclic::harness::v2::*;
    pub use super::generated::acyclic::protocol::v1::*;
}

/// Canonical harness descriptor set used for transport compatibility.
pub const FILE_DESCRIPTOR_SET: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/harness_descriptor.bin"));

#[cfg(feature = "grpc")]
/// Generated tonic client and server surfaces using the canonical Harness messages.
#[allow(
    clippy::pedantic,
    clippy::allow_attributes_without_reason,
    reason = "generated tonic bindings"
)]
pub mod transport {
    include!(concat!(env!("OUT_DIR"), "/grpc/acyclic.harness.v2.rs"));
}
