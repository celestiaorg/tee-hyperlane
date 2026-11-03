//! The bytes an ISM stores and the enclave attests to: `hyperlane_types::ism`, which voyager's
//! bridge lane decodes with the same code.

pub use hyperlane_types::ism::{
    AttestedUpdate, DecodeError, IsmState, ATTESTED_UPDATE_BYTES, ISM_STATE_BYTES,
};
