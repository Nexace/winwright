//! Conservative-by-default safety layer (spec §24–§26, §66).

mod policy;
mod redact;

pub use policy::Policy;
pub use redact::{REDACTED, SecretString, is_sensitive, redacted_value, summarize_text};
