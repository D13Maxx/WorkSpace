//! Context-building failures. Every one of these is user-facing: the builder
//! never truncates silently to avoid an error (master spec s6.2, s6.5).

use crate::model::ModelRef;
use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ContextError {
    #[error("conversation path is empty")]
    EmptyPath,

    #[error(
        "{target} has no room for input: a {context_window}-token window minus \
         {max_output} reserved for output, {system_tokens} for the system prompt \
         and a {margin}-token safety margin leaves nothing"
    )]
    NoInputBudget {
        target: ModelRef,
        context_window: u32,
        max_output: u32,
        system_tokens: u32,
        margin: u32,
    },

    #[error(
        "your latest message is about {message_tokens} tokens, but {target} can \
         only accept {available} tokens of input. Shorten it, or switch to a \
         model with a larger context window."
    )]
    LatestUserMessageTooLarge {
        target: ModelRef,
        message_tokens: u32,
        available: u32,
    },

    #[error(
        "the summary came back at {summary_tokens} tokens, over the {limit}-token \
         limit for this view"
    )]
    SummaryTooLarge { summary_tokens: u32, limit: u32 },

    #[error("conversation path contains no user message to answer")]
    NoUserMessage,
}
