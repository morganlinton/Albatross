use anyhow::{anyhow, Result};
use std::collections::{BTreeMap, BTreeSet};

use crate::app_state::AppState;
use crate::extensions::{
    configured_trust, spawn_configured, trust_extension, ExtensionTrustStatus,
};

use super::{command_list, DIM, GREEN, RESET, YELLOW};

pub(super) async fn cmd_extensions(args: &str, state: &mut AppState) -> Result<()> {
    let mut parts = args.split_whitespace();
    match parts.next() {
        None | Some("") | Some("list") => print_extensions(state),
        Some("trust") => {
            let name = parts
                .next()
                .ok_or_else(|| anyhow!("Usage: /extensions trust <name>"))?;
            trust_and_start(state, &[name.to_string()]).await?;
        }
        Some("trust-all") => {
            let names = state
                .config
                .extensions
                .iter()
                .filter(|(_, config)| config.enabled)
                .map(|(name, _)| name.clone())
                .collect::<Vec<_>>();
            trust_and_start(state, &names).await?;
        }
        Some(_) => println!("  {DIM}Usage: /extensions [list|trust <name>|trust-all]{RESET}"),
    }
    Ok(())
}

fn print_extensions(state: &AppState) {
    if state.config.extensions.is_empty() {
        println!("  {DIM}No extensions configured.{RESET}");
        return;
    }
    println!("  {GREEN}Extensions{RESET}");
    for entry in configured_trust(&state.config.extensions, &state.config.workspace_root) {
        let status = match entry.status {
            ExtensionTrustStatus::Trusted => format!("{GREEN}[trusted]{RESET}"),
            ExtensionTrustStatus::Modified => format!("{YELLOW}[modified]{RESET}"),
            ExtensionTrustStatus::Untrusted => format!("{YELLOW}[new]{RESET}"),
            ExtensionTrustStatus::Disabled => format!("{DIM}[disabled]{RESET}"),
        };
        println!("  {status} {} {DIM}{}{RESET}", entry.name, entry.hash);
    }
    for extension in state.extensions.loaded() {
        let version = extension
            .version
            .as_deref()
            .map(|value| format!(" v{value}"))
            .unwrap_or_default();
        println!(
            "  {GREEN}[loaded]{RESET} {}{version} {DIM}({} tools, {} commands, {} events){RESET}",
            extension.name,
            extension.tool_count,
            extension.command_count,
            extension.events.len()
        );
    }
}

async fn trust_and_start(state: &mut AppState, names: &[String]) -> Result<()> {
    let mut selected = BTreeMap::new();
    for name in names {
        let config = state
            .config
            .extensions
            .get(name)
            .ok_or_else(|| anyhow!("unknown extension: {name}"))?;
        if !config.enabled {
            return Err(anyhow!("extension `{name}` is disabled"));
        }
        trust_extension(&state.config.workspace_root, name, config)?;
        if !state
            .extensions
            .loaded()
            .iter()
            .any(|extension| extension.config_name == *name)
        {
            selected.insert(name.clone(), config.clone());
        }
    }

    let mut reserved = command_list()
        .into_iter()
        .map(|(name, _)| name)
        .collect::<BTreeSet<_>>();
    reserved.extend(
        state
            .extensions
            .command_list()
            .into_iter()
            .map(|(name, _)| name),
    );
    let (registry, mut errors) =
        spawn_configured(&selected, &state.config.workspace_root, &reserved).await;
    let loaded = registry.loaded().len();
    errors.extend(state.extensions.absorb(registry));
    for error in errors {
        println!("  {YELLOW}!{RESET} {DIM}extension: {error}{RESET}");
    }
    println!(
        "  {GREEN}✓{RESET} {DIM}trusted {} extension(s); started {loaded}{RESET}",
        names.len()
    );
    Ok(())
}
