//! Embeddable Albatross agent library.
//!
//! Most applications should start with [`sdk::AgentSession`]. The remaining
//! modules are kept private so the SDK can provide a deliberately small,
//! versioned compatibility surface over the CLI's internal agent loop.

// The binary still owns the full CLI entry point, so many shared internals are
// unused when this crate is compiled only as a library. The binary target is
// linted separately and remains the warning backstop for those modules.
#![allow(dead_code, unused_imports)]

mod agent;
mod agent_eval;
#[cfg(test)]
mod agent_integration_test;
mod anthropic;
mod app_state;
mod approval;
mod auth;
mod auto_loop;
mod backends;
mod banner;
mod batch_operations;
mod budget;
mod cancel;
mod capabilities;
mod catalog;
mod codex_oauth;
mod codex_responses;
mod commands;
mod config;
mod context_guard;
mod continuation;
mod crash_log;
mod diff_view;
mod dir_migration;
mod extensions;
mod fable_usage;
mod fix_loop;
mod handoff;
mod hardware;
mod hooks;
mod input;
mod iterate_loop;
mod loader;
mod markdown;
mod mcp;
mod model_system;
mod openai;
mod packages;
mod path_security;
mod planner;
mod playground;
mod project_memory;
mod prompt_library;
mod recommend;
mod renderer;
mod route_audit;
mod rubric;
mod scorecard;
pub mod sdk;
mod session;
mod session_paths;
mod session_turn;
mod setup;
mod shipcheck;
mod skills;
mod test_integration;
mod theme;
mod tools;
mod turn_checkpoint;
mod turn_trace;
mod update_check;
mod warmup;
mod xai_oauth;

pub use sdk::{AgentBuilder, AgentSession};
