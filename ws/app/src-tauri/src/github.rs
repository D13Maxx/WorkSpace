//! GitHub: push, pull, branches and clone (plain git, with git's own sign-in),
//! and, once signed in here, the branch's pull request and CI. The token sits
//! in the system keychain, never in a file.

use serde::Serialize;
use ws_secrets::{Secret, SecretStore};

type CmdResult<T> = Result<T, String>;

/// The keychain entry for the GitHub token.
pub const ACCOUNT: &str = "github";
const API: &str = "https://api.github.com";

async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> CmdResult<T> {
    tauri::async_runtime::spawn_blocking(f)
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn git_branches(root: String) -> CmdResult<ws_tools::git::Branches> {
    blocking(move || ws_tools::git::branches(&root)).await
}

#[tauri::command]
pub async fn git_switch(root: String, name: String, create: bool) -> CmdResult<()> {
    blocking(move || ws_tools::git::switch(&root, &name, create)).await
}

#[tauri::command]
pub async fn git_push(root: String) -> CmdResult<()> {
    blocking(move || ws_tools::git::push(&root)).await
}

#[tauri::command]
pub async fn git_pull(root: String) -> CmdResult<()> {
    blocking(move || ws_tools::git::pull(&root)).await
}

/// Clone into a new folder under `parent`; returns that folder.
#[tauri::command]
pub async fn git_clone(url: String, parent: String) -> CmdResult<String> {
    blocking(move || ws_tools::git::clone(&url, &parent)).await
}

/// The saved token, if any.
pub fn token() -> Option<String> {
    SecretStore::new()
        .get_key(ACCOUNT)
        .ok()
        .flatten()
        .map(|s| s.expose().to_owned())
}

fn client() -> CmdResult<reqwest::Client> {
    reqwest::Client::builder()
        .user_agent("Workspace")
        .timeout(std::time::Duration::from_secs(20))
        .build()
        .map_err(|e| e.to_string())
}

/// GET `path` from the API, with the token when there is one.
async fn get(path: &str, token: Option<&str>) -> CmdResult<serde_json::Value> {
    let mut req = client()?
        .get(format!("{API}{path}"))
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28");
    if let Some(t) = token {
        req = req.bearer_auth(t);
    }
    let res = req.send().await.map_err(|e| e.to_string())?;
    let status = res.status();
    let body: serde_json::Value = res.json().await.unwrap_or_default();
    if status.is_success() {
        Ok(body)
    } else {
        let why = body["message"].as_str().unwrap_or("no reason given");
        Err(format!("GitHub said {}: {why}", status.as_u16()))
    }
}

#[derive(Debug, Serialize)]
pub struct Account {
    pub login: String,
}

async fn account_for(token: &str) -> CmdResult<Account> {
    let user = get("/user", Some(token)).await?;
    let login = user["login"].as_str().ok_or("GitHub sent no user name")?;
    Ok(Account {
        login: login.to_owned(),
    })
}

/// Who Workspace is signed in to GitHub as; None when it is not.
#[tauri::command]
pub async fn github_account() -> CmdResult<Option<Account>> {
    match token() {
        Some(t) => account_for(&t).await.map(Some),
        None => Ok(None),
    }
}

/// Sign in with a pasted token (checked with GitHub first), or, with none,
/// with the GitHub sign-in git already has.
#[tauri::command]
pub async fn github_sign_in(token: Option<String>) -> CmdResult<Account> {
    let token = match token.map(|t| t.trim().to_owned()).filter(|t| !t.is_empty()) {
        Some(t) => t,
        None => {
            blocking(|| {
                ws_tools::git::github_credential().ok_or_else(|| {
                "git has no GitHub sign-in on this PC: push once from a terminal, or paste a token"
                    .to_owned()
            })
            })
            .await?
        }
    };
    let account = account_for(&token).await?;
    SecretStore::new()
        .set_key(ACCOUNT, &Secret::new(token))
        .map_err(|e| e.to_string())?;
    Ok(account)
}

/// Forget the token here; git's own sign-in is left alone.
#[tauri::command]
pub fn github_sign_out() -> CmdResult<()> {
    SecretStore::new()
        .delete_key(ACCOUNT)
        .map_err(|e| e.to_string())
}

#[derive(Debug, Default, Serialize)]
pub struct Pull {
    pub number: u64,
    pub title: String,
    pub url: String,
    /// open, closed or merged.
    pub state: String,
}

#[derive(Debug, Default, Serialize)]
pub struct Checks {
    pub passed: u32,
    pub failed: u32,
    pub running: u32,
}

/// The current branch on GitHub: its repo, its pull request and the CI of
/// the commit checked out.
#[derive(Debug, Default, Serialize)]
pub struct BranchOnGithub {
    /// `owner/repo`; None when `origin` is not on GitHub.
    pub repo: Option<String>,
    pub pull: Option<Pull>,
    pub checks: Option<Checks>,
}

#[tauri::command]
pub async fn github_branch(root: String) -> CmdResult<BranchOnGithub> {
    let (repo, branch, sha) = blocking(move || {
        let repo = ws_tools::git::github_repo(&root);
        let branch = ws_tools::git::branches(&root)?.current;
        let sha = ws_tools::git::head(&root).unwrap_or_default();
        Ok((repo, branch, sha))
    })
    .await?;
    let Some(repo) = repo else {
        return Ok(BranchOnGithub::default());
    };
    let token = token();
    let token = token.as_deref();
    let owner = repo.split('/').next().unwrap_or_default();
    let mut pull = None;
    if !branch.is_empty() {
        let list = get(
            &format!(
                "/repos/{repo}/pulls?state=all&per_page=1&head={}",
                encode(&format!("{owner}:{branch}"))
            ),
            token,
        )
        .await?;
        pull = list.get(0).map(|p| Pull {
            number: p["number"].as_u64().unwrap_or(0),
            title: p["title"].as_str().unwrap_or_default().to_owned(),
            url: p["html_url"].as_str().unwrap_or_default().to_owned(),
            state: if p["merged_at"].is_string() {
                "merged".to_owned()
            } else {
                p["state"].as_str().unwrap_or_default().to_owned()
            },
        });
    }
    // A commit not pushed yet has no CI: GitHub answers 422, shown as none.
    let checks = if sha.is_empty() {
        None
    } else {
        get(&format!("/repos/{repo}/commits/{sha}/check-runs"), token)
            .await
            .ok()
            .map(|runs| count_checks(&runs))
            .filter(|c| c.passed + c.failed + c.running > 0)
    };
    Ok(BranchOnGithub {
        repo: Some(repo),
        pull,
        checks,
    })
}

/// Where the agent-tools switch is kept, in the app's data folder.
const AGENT_TOOLS_FILE: &str = "github-agent-tools";
/// GitHub's own tool server (github.com/github/github-mcp-server).
const SERVER: &str = "github-mcp-server";

fn server_path() -> Option<std::path::PathBuf> {
    ws_tools::find(&[SERVER])
}

/// GitHub's tool server for the agent, when the switch is on, the server is
/// installed and Workspace is signed in. The token goes in at start only.
pub fn agent_server(data_dir: &std::path::Path) -> Option<ws_mcp::ServerConfig> {
    if !data_dir.join(AGENT_TOOLS_FILE).exists() {
        return None;
    }
    let command = server_path()?.to_string_lossy().into_owned();
    let token = token()?;
    Some(ws_mcp::ServerConfig {
        command,
        args: vec!["stdio".to_owned()],
        env: [("GITHUB_PERSONAL_ACCESS_TOKEN".to_owned(), token)].into(),
        auto_approve: Vec::new(),
    })
}

#[derive(Debug, Serialize)]
pub struct AgentTools {
    pub enabled: bool,
    /// github-mcp-server is on PATH.
    pub installed: bool,
}

#[tauri::command]
pub fn github_agent_tools(state: tauri::State<'_, crate::state::AppState>) -> AgentTools {
    AgentTools {
        enabled: state.data_dir.join(AGENT_TOOLS_FILE).exists(),
        installed: server_path().is_some(),
    }
}

/// Turn the agent's GitHub tools on or off; tool servers start with the app,
/// so it takes effect at the next start.
#[tauri::command]
pub fn set_github_agent_tools(
    state: tauri::State<'_, crate::state::AppState>,
    enabled: bool,
) -> CmdResult<()> {
    let flag = state.data_dir.join(AGENT_TOOLS_FILE);
    if enabled {
        std::fs::write(flag, "on\n").map_err(|e| e.to_string())
    } else if flag.exists() {
        std::fs::remove_file(flag).map_err(|e| e.to_string())
    } else {
        Ok(())
    }
}

/// Percent-encode for a query value (branch names may hold `&` or `#`).
fn encode(s: &str) -> String {
    s.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-._/:".contains(&b) {
                char::from(b).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

fn count_checks(runs: &serde_json::Value) -> Checks {
    let mut c = Checks::default();
    for run in runs["check_runs"].as_array().into_iter().flatten() {
        match (run["status"].as_str(), run["conclusion"].as_str()) {
            (Some("completed"), Some("success" | "skipped" | "neutral")) => c.passed += 1,
            (Some("completed"), _) => c.failed += 1,
            _ => c.running += 1,
        }
    }
    c
}

/// Open a GitHub page in the browser; only `https://github.com/` addresses.
#[tauri::command]
pub fn open_github(url: String) -> CmdResult<()> {
    if !url.starts_with("https://github.com/") || url.chars().any(|c| c.is_whitespace() || c == '"')
    {
        return Err("only github.com pages open from here".to_owned());
    }
    #[cfg(windows)]
    let mut cmd = std::process::Command::new("explorer");
    #[cfg(not(windows))]
    let mut cmd = std::process::Command::new("xdg-open");
    cmd.arg(&url).spawn().map(drop).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn checks_are_counted_by_outcome() {
        let runs = serde_json::json!({ "check_runs": [
            { "status": "completed", "conclusion": "success" },
            { "status": "completed", "conclusion": "skipped" },
            { "status": "completed", "conclusion": "failure" },
            { "status": "in_progress", "conclusion": null },
        ]});
        let c = count_checks(&runs);
        assert_eq!((c.passed, c.failed, c.running), (2, 1, 1));
        assert_eq!(encode("me:fix&go #1"), "me:fix%26go%20%231");
        assert!(open_github("https://evil.example/".to_owned()).is_err());
        assert!(open_github("https://github.com/a b".to_owned()).is_err());
    }
}
