use anyhow::Result;

use crate::app_state::AppState;
use crate::packages::list_installed;
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
    let skills = &state.config.skills;
    if skills.is_empty() {
        println!("  {DIM}No valid Agent Skills discovered.{RESET}");
        for diagnostic in &skills.diagnostics {
            println!("  {YELLOW}!{RESET} {DIM}{diagnostic}{RESET}");
        }
        return;
    }
    println!("  {GREEN}Agent Skills{RESET}");
    for skill in skills.skills() {
        println!(
            "  {CYAN}/skill:{}{RESET} {DIM}{} · {} · {}{RESET}",
            skill.name,
            skill.scope.as_str(),
            skill.description,
            skill.path.display()
        );
    }
    for diagnostic in &skills.diagnostics {
        println!("  {YELLOW}!{RESET} {DIM}{diagnostic}{RESET}");
    }
}

pub(super) fn skill_command_list(state: &AppState) -> Vec<(String, String)> {
    state.config.skills.command_entries()
}

pub(super) async fn execute_skill(command: &str, args: &str, state: &mut AppState) -> Result<bool> {
    let Some(name) = command.strip_prefix("/skill:") else {
        return Ok(false);
    };
    let instructions = state.config.skills.activate(name)?;
    let task = args.trim();
    let prompt = if task.is_empty() {
        format!("The user explicitly activated Agent Skill `{name}`. Apply it to the current task and conversation.\n\n{instructions}")
    } else {
        format!("The user explicitly activated Agent Skill `{name}` for this request:\n{task}\n\n{instructions}")
    };
    run_user_turn(
        state,
        TurnOptions {
            user_prompt: prompt,
            auto_verify_tests: state.config.mode == crate::config::OperatorMode::Ship,
            yolo_approve: false,
            source: "agent-skill",
        },
    )
    .await?;
    Ok(true)
}
