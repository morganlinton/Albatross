use anyhow::{anyhow, Result};

use crate::app_state::AppState;
use crate::packages::{list_installed, read_text_resource};
use crate::session_turn::{run_user_turn, TurnOptions};

use super::{CYAN, DIM, GREEN, RESET, YELLOW};

pub(super) fn cmd_packages(args: &str, state: &AppState) -> Result<()> {
    if !args.trim().is_empty() && args.trim() != "list" {
        println!("  {DIM}Package mutations are top-level commands:{RESET}");
        println!("  {CYAN}albatross install npm:<package>{RESET}");
        println!("  {CYAN}albatross install git:<url>{RESET}");
        println!("  {CYAN}albatross update [package]{RESET}");
        println!("  {CYAN}albatross remove <package>{RESET}");
        return Ok(());
    }
    let packages = list_installed()?;
    if packages.is_empty() {
        println!("  {DIM}No packages installed.{RESET}");
        return Ok(());
    }
    println!("  {GREEN}Packages{RESET}");
    for package in packages {
        let pin = if package.pinned { " · pinned" } else { "" };
        println!(
            "  {CYAN}{}{RESET} {DIM}{}{}{RESET}",
            package.id, package.source, pin
        );
    }
    let resources = &state.config.package_resources;
    println!(
        "  {DIM}{} extension(s) · {} skill(s) · {} prompt(s) · {} theme(s){RESET}",
        resources.extensions.len(),
        resources.skills.len(),
        resources.prompts.len(),
        resources.themes.len()
    );
    for (name, theme) in &resources.themes {
        println!(
            "  {DIM}theme {RESET}{CYAN}{name}{RESET} {DIM}from {} · {}{RESET}",
            theme.package,
            theme.path.display()
        );
    }
    for diagnostic in &resources.diagnostics {
        println!("  {YELLOW}!{RESET} {DIM}{diagnostic}{RESET}");
    }
    Ok(())
}

pub(super) fn cmd_skills(state: &AppState) {
    let skills = &state.config.package_resources.skills;
    if skills.is_empty() {
        println!("  {DIM}No packaged skills installed.{RESET}");
        return;
    }
    println!("  {GREEN}Packaged skills{RESET}");
    for skill in skills.values() {
        let detail = if skill.description.is_empty() {
            String::new()
        } else {
            format!(" — {}", skill.description)
        };
        println!("  {CYAN}/skill:{}{RESET}{DIM}{detail}{RESET}", skill.name);
    }
}

pub(super) fn skill_command_list(state: &AppState) -> Vec<(String, String)> {
    state
        .config
        .package_resources
        .skills
        .values()
        .map(|skill| {
            (
                format!("/skill:{}", skill.name),
                if skill.description.is_empty() {
                    format!("Activate skill from {}", skill.package)
                } else {
                    skill.description.clone()
                },
            )
        })
        .collect()
}

pub(super) async fn execute_skill(command: &str, args: &str, state: &mut AppState) -> Result<bool> {
    let Some(name) = command.strip_prefix("/skill:") else {
        return Ok(false);
    };
    let skill = state
        .config
        .package_resources
        .skills
        .get(name)
        .cloned()
        .ok_or_else(|| anyhow!("unknown packaged skill: {name}"))?;
    let instructions = read_text_resource(&skill)?;
    let task = args.trim();
    let prompt = if task.is_empty() {
        format!("Apply the following skill instructions to the current task and conversation.\n\n<skill name=\"{}\">\n{}\n</skill>", skill.name, instructions)
    } else {
        format!("Apply the following skill instructions to this request:\n{task}\n\n<skill name=\"{}\">\n{}\n</skill>", skill.name, instructions)
    };
    run_user_turn(
        state,
        TurnOptions {
            user_prompt: prompt,
            auto_verify_tests: state.config.mode == crate::config::OperatorMode::Ship,
            yolo_approve: false,
            source: "package-skill",
        },
    )
    .await?;
    Ok(true)
}
