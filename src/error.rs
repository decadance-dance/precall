//! Errors that carry an exit code of spec §2. Any other error exits 1.

use std::fmt;

#[derive(Debug)]
pub struct Fail {
    pub code: u8,
    pub msg: String,
}

impl fmt::Display for Fail {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.msg)
    }
}

impl std::error::Error for Fail {}

pub fn not_found(msg: impl Into<String>) -> anyhow::Error {
    Fail {
        code: 1,
        msg: msg.into(),
    }
    .into()
}

pub fn usage(msg: impl Into<String>) -> anyhow::Error {
    Fail {
        code: 2,
        msg: msg.into(),
    }
    .into()
}

pub fn no_map(msg: impl Into<String>) -> anyhow::Error {
    Fail {
        code: 3,
        msg: msg.into(),
    }
    .into()
}

/// A run aborted before its commit: a manifest or config error, a hook's deny (spec §2).
pub fn aborted(msg: impl Into<String>) -> anyhow::Error {
    Fail {
        code: 4,
        msg: msg.into(),
    }
    .into()
}

/// The process exit code for an error.
pub fn exit_code(e: &anyhow::Error) -> u8 {
    e.downcast_ref::<Fail>().map_or(1, |f| f.code)
}
