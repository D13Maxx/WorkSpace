//! Adding skills and tool servers from Settings: write a skill, clone a repo
//! of them, remove one, and edit `mcp.json` (a form, or pasted config), then
//! pick the change up without restarting Workspace.

use crate::state::AppState;
use serde::Serialize;
use tauri::State;

type CmdResult<T> = Result<T, String>;

#[derive(Debug, Serialize)]
pub struct SkillDto {
    pub name: String,
    pub description: String,
    /// In the user's skills folder, so Settings can remove it.
    pub own: bool,
}

#[derive(Debug, Serialize)]
pub struct SkillsDto {
    /// Where the user's skills live.
    pub dir: String,
    pub skills: Vec<SkillDto>,
}

#[tauri::command]
pub fn list_skills(state: State<'_, AppState>) -> SkillsDto {
    let dir = &state.skills.dir;
    SkillsDto {
        dir: dir.display().to_string(),
        skills: state
            .skills
            .current()
            .list()
            .iter()
            .map(|s| SkillDto {
                name: s.name.clone(),
                description: s.description.clone(),
                own: s.dir.starts_with(dir) && s.dir != *dir,
            })
            .collect(),
    }
}

/// Write a new skill into the skills folder and offer it at once.
#[tauri::command]
pub fn create_skill(
    state: State<'_, AppState>,
    name: String,
    description: String,
    instructions: String,
) -> CmdResult<SkillsDto> {
    let text = ws_core::skills::skill_file(&name, &description, &instructions)?;
    let folder = state.skills.dir.join(name.trim());
    if folder.exists() {
        return Err(format!(
            "a skill folder called {} is there already",
            name.trim()
        ));
    }
    std::fs::create_dir_all(&folder).map_err(|e| e.to_string())?;
    std::fs::write(folder.join("SKILL.md"), text).map_err(|e| e.to_string())?;
    state.skills.reload();
    Ok(list_skills(state))
}

/// Clone a repo of skills (owner/repo or an address) into the skills folder.
#[tauri::command]
pub async fn add_skill_repo(state: State<'_, AppState>, url: String) -> CmdResult<SkillsDto> {
    let dir = state.skills.dir.to_string_lossy().into_owned();
    tauri::async_runtime::spawn_blocking(move || ws_tools::git::clone(&url, &dir))
        .await
        .map_err(|e| e.to_string())??;
    state.skills.reload();
    Ok(list_skills(state))
}

/// Move one of the user's skills to the Recycle Bin. Built-in ones stay.
#[tauri::command]
pub fn remove_skill(state: State<'_, AppState>, name: String) -> CmdResult<SkillsDto> {
    let dir = &state.skills.dir;
    let skills = state.skills.current();
    let skill = skills
        .get(&name)
        .ok_or_else(|| format!("there is no skill called {name}"))?;
    let rel = skill
        .dir
        .strip_prefix(dir)
        .ok()
        .filter(|r| !r.as_os_str().is_empty())
        .ok_or_else(|| format!("{name} is built in, so it cannot be removed"))?;
    ws_tools::files::delete(&dir.to_string_lossy(), &rel.to_string_lossy())?;
    state.skills.reload();
    Ok(list_skills(state))
}

/// Look in the skills folder again, for skills added by hand.
#[tauri::command]
pub fn reload_skills(state: State<'_, AppState>) -> SkillsDto {
    state.skills.reload();
    list_skills(state)
}

fn mcp_file(state: &AppState) -> Option<String> {
    std::fs::read_to_string(&state.mcp.config_path).ok()
}

fn write_mcp(state: &AppState, text: &str) -> CmdResult<()> {
    std::fs::write(&state.mcp.config_path, text).map_err(|e| e.to_string())
}

/// The servers in `mcp.json`, as written there, for the form to show.
#[tauri::command]
pub fn mcp_servers(state: State<'_, AppState>) -> CmdResult<serde_json::Value> {
    let Some(text) = mcp_file(&state).filter(|t| !t.trim().is_empty()) else {
        return Ok(serde_json::json!({}));
    };
    let v: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("mcp.json is not valid JSON: {e}"))?;
    Ok(v.get("mcpServers")
        .cloned()
        .unwrap_or_else(|| serde_json::json!({})))
}

/// Add or change one server; `was` is its old name when it is renamed.
#[tauri::command]
pub fn save_mcp_server(
    state: State<'_, AppState>,
    id: String,
    server: serde_json::Value,
    was: Option<String>,
) -> CmdResult<()> {
    let id = id.trim().to_owned();
    let mut text = ws_mcp::edit_config(mcp_file(&state).as_deref(), &id, Some(server))?;
    if let Some(old) = was.filter(|w| *w != id) {
        text = ws_mcp::edit_config(Some(&text), &old, None)?;
    }
    write_mcp(&state, &text)
}

#[tauri::command]
pub fn remove_mcp_server(state: State<'_, AppState>, id: String) -> CmdResult<()> {
    let text = ws_mcp::edit_config(mcp_file(&state).as_deref(), &id, None)?;
    write_mcp(&state, &text)
}

/// Add the servers in config pasted from a tool's README; returns their names.
#[tauri::command]
pub fn add_mcp_servers(state: State<'_, AppState>, text: String) -> CmdResult<Vec<String>> {
    let servers = ws_mcp::pasted_servers(&text)?;
    let mut file = mcp_file(&state);
    for (id, server) in &servers {
        file = Some(ws_mcp::edit_config(
            file.as_deref(),
            id,
            Some(server.clone()),
        )?);
    }
    if let Some(text) = file {
        write_mcp(&state, &text)?;
    }
    Ok(servers.keys().cloned().collect())
}

/// Stop the tool servers and start them again from `mcp.json`.
#[tauri::command]
pub async fn restart_mcp(state: State<'_, AppState>) -> CmdResult<Vec<ws_mcp::ServerStatus>> {
    state.mcp.reconnect().await;
    Ok(state.mcp.status())
}
