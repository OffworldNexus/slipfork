//! Build-side adapter over the one identity owner in `slipfork-world`.
//!
//! The rules live in [`slipfork_world::identity`]; this module only keeps the build's error
//! vocabulary (`UnknownAccount`/`StepFailed`) stable for `Dockerfile` diagnostics.

use slipfork_world::identity::IdentityError;
pub(crate) use slipfork_world::identity::{Identity, PROCESS_UMASK};

use crate::error::Error;

impl From<IdentityError> for Error {
    fn from(error: IdentityError) -> Self {
        match error {
            IdentityError::UnknownAccount { account, file } => {
                Self::UnknownAccount { account, file }
            }
            IdentityError::Invalid { spec } => Self::StepFailed {
                line: 0,
                phase: "user",
                instruction: String::new().into(),
                message: format!("`{spec}` is not a valid user spec"),
                exit_code: None,
                runtime: None,
                next: "write `<name>[:<group>]` or a numeric `<uid>[:<gid>]`".to_owned(),
            },
        }
    }
}
