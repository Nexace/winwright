//! Conservative-by-default safety layer (spec §24–§26, §66).

mod classify;
mod policy;
mod redact;

pub use classify::{classify_activation, program_capability};
pub use policy::Policy;
pub use redact::{REDACTED, SecretString, is_sensitive, redacted_value, summarize_text};
