//! dfu-util integration: argv construction and output classification.

pub mod args;
pub mod parser;

pub use args::{Invocation, Op};
pub use parser::{
    BenignKind, Ctx, DfuEvent, DfuOutcome, Failure, FlashLayout, Milestone, ProgressOp, Seen,
};
