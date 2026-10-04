//! Conservative-by-default safety layer (spec §24–§26, §66).

mod classify;
mod policy;
mod redact;

pub use classify::{
    classify_activation, classify_submit, command_capability, console_capability, is_affirmative,
    is_launcher, is_secret_path, is_terminal_field, opened_capability, program_capability,
    stricter, transfer_risk,
};
pub use policy::Policy;
pub use redact::{REDACTED, SecretString, is_sensitive, redacted_value, summarize_text};
