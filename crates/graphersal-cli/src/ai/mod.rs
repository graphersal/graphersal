//! The AI Chat of the dev server (`graphersal --server --ai FILE`, feature `ai`):
//! **EXPERIMENTAL**, like the dev server.
//!
//! - [`config`]: the `--ai FILE` JSON (provider, model, key from an environment variable,
//!   limits), read strictly;
//! - [`client`]: the provider-neutral conversation model and the one call
//!   [`client::Client::complete`];
//! - [`history`]: a conversation's stored messages and events, compressed per finished turn;
//! - `openai` / `anthropic`: the two provider adapters (Chat Completions with tools; the
//!   Messages API with tool use and prompt caching of the static system prefix), over plain
//!   HTTPS with a synchronous client (`ureq` + rustls; no async runtime, no SDK).
//!
//! The API key stays in this process: it is never logged, never part of an error text and never
//! sent anywhere but to the configured provider.

//! The agent loop and the `/api/ai/*` endpoints that use the client are in `server/ai.rs`.

mod anthropic;
pub mod client;
pub mod config;
pub mod history;
mod openai;

#[cfg(test)]
mod tests;
