//! Conservative-by-default safety layer (spec §24–§26, §66).

mod classify;
mod policy;
mod redact;

pub use classify::{
    classify_activation, classify_submit, is_affirmative, program_capability, transfer_risk,
};
pub use policy::Policy;
pub use redact::{REDACTED, SecretString, is_sensitive, redacted_value, summarize_text};
