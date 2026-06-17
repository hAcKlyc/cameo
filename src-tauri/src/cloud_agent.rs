use crate::codex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin};
use tokio::sync::{mpsc, oneshot, Mutex as TokioMutex};

const CLOUD_AGENT_EVENT: &str = "cloud-agent-event";

#[derive(Default)]
pub struct CloudAgentState {
    task: TokioMutex<Option<tokio::task::JoinHandle<()>>>,
    active_jobs: TokioMutex<HashMap<String, ActiveJob>>,
    job_tasks: TokioMutex<HashMap<String, tokio::task::JoinHandle<()>>>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActiveJob {
    pub job_id: String,
    pub workspace_id: String,
    pub execution_session_id: String,
    pub phase: String,
    pub started_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CloudAgentConfig {
    pub api_base: String,
    pub agent_id: String,
    pub team_id: String,
    #[serde(default)]
    pub refresh_token: Option<String>,
    pub name: String,
    pub max_concurrent_sessions: u32,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CloudAgentStatus {
    pub configured: bool,
    pub running: bool,
    pub api_base: Option<String>,
    pub agent_id: Option<String>,
    pub team_id: Option<String>,
    pub name: Option<String>,
    pub max_concurrent_sessions: u32,
    pub active_session_count: u32,
    pub active_jobs: Vec<ActiveJob>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PairResponse {
    pub agent_id: String,
    pub team_id: String,
    pub refresh_token: String,
    pub access_token: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TokenResponse {
    access_token: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct JobsResponse {
    jobs: Vec<JobSummary>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct JobSummary {
    id: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LeaseResponse {
    job: AgentJob,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ManifestResponse {
    job: AgentJob,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AgentJob {
    id: String,
    team_id: String,
    workspace_id: String,
    execution_session_id: String,
    status: String,
    prompt: String,
    references: Vec<CloudTurnReference>,
    gen_settings: CloudGenSettings,
    workspace: WorkspaceManifest,
    agent_session_ref: Option<String>,
    assets: Vec<CloudAsset>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CloudTurnReference {
    placement_id: String,
    asset_id: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CloudGenSettings {
    model: String,
    effort: String,
    service_tier: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WorkspaceManifest {
    revision: u64,
    board_doc: Value,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CloudAsset {
    id: String,
    object_key: String,
    filename: String,
    mime: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UploadUrlResponse {
    asset_id: String,
    upload_url: Option<String>,
    fallback_upload_url: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CloudAgentEvent {
    pub kind: String,
    pub message: String,
}

pub fn config_path() -> PathBuf {
    crate::paths::cameo_data_dir()
        .join("cloud")
        .join("agent.json")
}

pub fn cloud_root() -> PathBuf {
    crate::paths::cameo_data_dir().join("cloud")
}

fn emit(app: &AppHandle, kind: &str, message: impl Into<String>) {
    let _ = app.emit(
        CLOUD_AGENT_EVENT,
        CloudAgentEvent {
            kind: kind.to_string(),
            message: message.into(),
        },
    );
}

pub fn load_config() -> Option<CloudAgentConfig> {
    let bytes = std::fs::read(config_path()).ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn save_config(config: &CloudAgentConfig) -> Result<(), String> {
    let path = config_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let bytes = serde_json::to_vec_pretty(config).map_err(|e| e.to_string())?;
    std::fs::write(&path, bytes).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

pub async fn status(state: Arc<CloudAgentState>) -> CloudAgentStatus {
    let cfg = load_config();
    let running = state.task.lock().await.as_ref().is_some_and(|h| !h.is_finished());
    let active_jobs: Vec<ActiveJob> = state.active_jobs.lock().await.values().cloned().collect();
    CloudAgentStatus {
        configured: cfg.is_some(),
        running,
        api_base: cfg.as_ref().map(|c| c.api_base.clone()),
        agent_id: cfg.as_ref().map(|c| c.agent_id.clone()),
        team_id: cfg.as_ref().map(|c| c.team_id.clone()),
        name: cfg.as_ref().map(|c| c.name.clone()),
        max_concurrent_sessions: cfg.as_ref().map(|c| c.max_concurrent_sessions).unwrap_or(6),
        active_session_count: active_jobs.len() as u32,
        active_jobs,
    }
}

pub async fn pair(
    app: AppHandle,
    state: Arc<CloudAgentState>,
    api_base: String,
    pairing_code: String,
    name: String,
    max_concurrent_sessions: u32,
) -> Result<CloudAgentStatus, String> {
    let api_base = api_base.trim().trim_end_matches('/').to_string();
    if !api_base.starts_with("http://") && !api_base.starts_with("https://") {
        return Err("Cloud API base must start with http:// or https://".into());
    }
    let max = max_concurrent_sessions.clamp(1, 24);
    let body = json!({
        "pairingCode": pairing_code.trim(),
        "name": name.trim(),
        "maxConcurrentSessions": max,
        "appVersion": env!("CARGO_PKG_VERSION"),
        "platform": std::env::consts::OS,
    });
    let res: PairResponse = api_json(&api_base, "/api/v1/agents/register", None, "POST", Some(body)).await?;
    let file_refresh_token = match save_refresh_token(&res.agent_id, &res.refresh_token) {
        Ok(()) => None,
        Err(e) => {
            tracing::warn!(module = "cloud_agent", error = %e, "falling back to file-stored refresh token");
            Some(res.refresh_token.clone())
        }
    };
    let cfg = CloudAgentConfig {
        api_base,
        agent_id: res.agent_id,
        team_id: res.team_id,
        refresh_token: file_refresh_token,
        name: if name.trim().is_empty() { "Cameo Agent".into() } else { name.trim().into() },
        max_concurrent_sessions: max,
    };
    save_config(&cfg)?;
    start(app, state.clone()).await?;
    Ok(status(state).await)
}

pub async fn start(app: AppHandle, state: Arc<CloudAgentState>) -> Result<(), String> {
    let cfg = load_config().ok_or("Cloud Agent is not paired")?;
    let mut guard = state.task.lock().await;
    if guard.as_ref().is_some_and(|h| !h.is_finished()) {
        return Ok(());
    }
    let handle = tokio::spawn(run_loop(app, cfg, state.clone()));
    *guard = Some(handle);
    Ok(())
}

pub async fn stop(state: Arc<CloudAgentState>) {
    if let Some(handle) = state.task.lock().await.take() {
        handle.abort();
    }
    let tasks: Vec<tokio::task::JoinHandle<()>> = state.job_tasks.lock().await.drain().map(|(_, task)| task).collect();
    for task in tasks {
        task.abort();
    }
    state.active_jobs.lock().await.clear();
}

pub fn start_if_configured(app: AppHandle) {
    if !cloud_enabled() {
        return;
    }
    if load_config().is_none() {
        return;
    }
    let state = app.state::<Arc<CloudAgentState>>().inner().clone();
    tauri::async_runtime::spawn(async move {
        let _ = start(app, state).await;
    });
}

pub fn cloud_enabled() -> bool {
    cfg!(debug_assertions)
        || option_env!("CAMEO_CLOUD_ENABLED").is_some_and(|value| value == "true")
        || option_env!("VITE_CAMEO_CLOUD_ENABLED").is_some_and(|value| value == "true")
}

async fn run_loop(app: AppHandle, cfg: CloudAgentConfig, state: Arc<CloudAgentState>) {
    emit(&app, "status", "Cloud Agent started");
    let mut access_token: Option<String> = None;
    loop {
        let result = async {
            let token = match access_token.clone() {
                Some(token) => token,
                None => {
                    let token = refresh_access_token(&cfg).await?;
                    access_token = Some(token.clone());
                    token
                }
            };
            let active_snapshot: Vec<ActiveJob> = state.active_jobs.lock().await.values().cloned().collect();
            heartbeat(&cfg, &token, &active_snapshot).await?;
            let available_slots = cfg
                .max_concurrent_sessions
                .saturating_sub(active_snapshot.len() as u32) as usize;
            if available_slots == 0 {
                return Ok::<(), String>(());
            }
            let jobs: JobsResponse = api_json(&cfg.api_base, "/api/v1/agent/jobs", Some(&token), "GET", None).await?;
            for job in jobs.jobs.into_iter().take(available_slots) {
                if state.active_jobs.lock().await.contains_key(&job.id) {
                    continue;
                }
                let app_for_job = app.clone();
                let cfg_for_job = cfg.clone();
                let token_for_job = token.clone();
                let state_for_job = state.clone();
                let job_id = job.id.clone();
                let handle = tokio::spawn(async move {
                    let result = run_job(&app_for_job, &cfg_for_job, &token_for_job, &job_id, state_for_job.clone()).await;
                    if let Err(e) = result {
                        let status = if e == "cancelled" { "cancelled" } else { "failed" };
                        let _ = complete_job(&cfg_for_job, &token_for_job, &job_id, status, Some(&e), None, &[]).await;
                        emit(&app_for_job, "error", format!("job {} {status}: {e}", job_id));
                    }
                    state_for_job.active_jobs.lock().await.remove(&job_id);
                    state_for_job.job_tasks.lock().await.remove(&job_id);
                });
                state.job_tasks.lock().await.insert(job.id, handle);
            }
            Ok::<(), String>(())
        }
        .await;
        if let Err(e) = result {
            emit(&app, "error", e);
            access_token = None;
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
}

async fn refresh_access_token(cfg: &CloudAgentConfig) -> Result<String, String> {
    let refresh_token = load_refresh_token(cfg)?;
    let res: TokenResponse = api_json(
        &cfg.api_base,
        "/api/v1/agents/token",
        None,
        "POST",
        Some(json!({ "agentId": cfg.agent_id, "refreshToken": refresh_token })),
    )
    .await?;
    Ok(res.access_token)
}

fn save_refresh_token(agent_id: &str, refresh_token: &str) -> Result<(), String> {
    let entry = keyring::Entry::new("cameo-cloud", agent_id).map_err(|e| e.to_string())?;
    entry.set_password(refresh_token).map_err(|e| e.to_string())
}

fn load_refresh_token(cfg: &CloudAgentConfig) -> Result<String, String> {
    if let Ok(entry) = keyring::Entry::new("cameo-cloud", &cfg.agent_id) {
        if let Ok(token) = entry.get_password() {
            return Ok(token);
        }
    }
    cfg.refresh_token
        .clone()
        .ok_or_else(|| "Cloud Agent credential is missing; pair this machine again".into())
}

async fn heartbeat(cfg: &CloudAgentConfig, token: &str, active_jobs: &[ActiveJob]) -> Result<(), String> {
    let _: Value = api_json(
        &cfg.api_base,
        "/api/v1/agent/heartbeat",
        Some(token),
        "POST",
        Some(json!({
            "maxConcurrentSessions": cfg.max_concurrent_sessions,
            "activeSessionCount": active_jobs.len(),
            "activeJobs": active_jobs,
            "queueDepth": 0,
            "permissionCeiling": "workspace",
            "codexAuthReady": true,
            "appVersion": env!("CARGO_PKG_VERSION"),
            "platform": std::env::consts::OS,
        })),
    )
    .await?;
    Ok(())
}

async fn run_job(
    app: &AppHandle,
    cfg: &CloudAgentConfig,
    token: &str,
    job_id: &str,
    state: Arc<CloudAgentState>,
) -> Result<(), String> {
    emit(app, "job", format!("leasing {job_id}"));
    let leased: LeaseResponse = api_json(
        &cfg.api_base,
        &format!("/api/v1/agent/jobs/{job_id}/lease"),
        Some(token),
        "POST",
        None,
    )
    .await?;
    upsert_active_job(&state, &leased.job, "syncing").await;
    set_job_status(cfg, token, job_id, "syncing", None).await?;
    let manifest: ManifestResponse = api_json(
        &cfg.api_base,
        &format!("/api/v1/agent/jobs/{job_id}/manifest"),
        Some(token),
        "GET",
        None,
    )
    .await?;
    let job = manifest.job;
    upsert_active_job(&state, &job, "syncing").await;
    let workspace_dir = sync_workspace(cfg, token, &job).await?;
    let overlays = render_reference_overlays(&workspace_dir, &job)?;
    upsert_active_job(&state, &job, "running").await;
    set_job_status(cfg, token, job_id, "running", None).await?;
    let before = media_files(&workspace_dir)?;
    let mut runner = CloudCodexRunner::start(&workspace_dir).await?;
    let agent_session_ref = runner
        .run_turn(
            cfg,
            token,
            &job,
            build_prompt(&job, &workspace_dir, &overlays),
            leased.job.agent_session_ref,
        )
        .await?;
    upsert_active_job(&state, &job, "uploading").await;
    set_job_status(cfg, token, job_id, "uploading", None).await?;
    let after = media_files(&workspace_dir)?;
    let outputs: Vec<PathBuf> = after.difference(&before).cloned().collect();
    let committed = upload_outputs(cfg, token, &job, &outputs).await?;
    complete_job(cfg, token, job_id, "completed", None, Some(&agent_session_ref), &committed).await?;
    emit(app, "job", format!("completed {job_id}"));
    Ok(())
}

async fn upsert_active_job(state: &Arc<CloudAgentState>, job: &AgentJob, phase: &str) {
    let started_at = {
        let existing = state.active_jobs.lock().await.get(&job.id).cloned();
        existing.map(|job| job.started_at).unwrap_or_else(now_rfc3339)
    };
    state.active_jobs.lock().await.insert(
        job.id.clone(),
        ActiveJob {
            job_id: job.id.clone(),
            workspace_id: job.workspace_id.clone(),
            execution_session_id: job.execution_session_id.clone(),
            phase: phase.to_string(),
            started_at,
        },
    );
}

async fn complete_job(
    cfg: &CloudAgentConfig,
    token: &str,
    job_id: &str,
    status: &str,
    error: Option<&str>,
    agent_session_ref: Option<&str>,
    outputs: &[Value],
) -> Result<(), String> {
    let _: Value = api_json(
        &cfg.api_base,
        &format!("/api/v1/agent/jobs/{job_id}/complete"),
        Some(token),
        "POST",
        Some(json!({ "status": status, "error": error, "agentSessionRef": agent_session_ref, "outputs": outputs })),
    )
    .await?;
    Ok(())
}

fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339()
}

async fn set_job_status(
    cfg: &CloudAgentConfig,
    token: &str,
    job_id: &str,
    status: &str,
    error: Option<&str>,
) -> Result<(), String> {
    let _: Value = api_json(
        &cfg.api_base,
        &format!("/api/v1/agent/jobs/{job_id}/status"),
        Some(token),
        "POST",
        Some(json!({ "status": status, "error": error })),
    )
    .await?;
    Ok(())
}

fn workspace_dir(team_id: &str, workspace_id: &str) -> PathBuf {
    cloud_root()
        .join("teams")
        .join(team_id)
        .join("workspaces")
        .join(workspace_id)
}

fn cloud_json_path(root: &Path) -> PathBuf {
    root.join(".cameo").join("cloud.json")
}

fn read_cloud_json(root: &Path) -> Option<Value> {
    let bytes = std::fs::read(cloud_json_path(root)).ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn save_cloud_json(root: &Path, value: &Value) -> Result<(), String> {
    let path = cloud_json_path(root);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    std::fs::write(path, serde_json::to_vec_pretty(value).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())
}

fn thread_id_for_session(root: &Path, session_ref: &str) -> Option<String> {
    read_cloud_json(root)?
        .get("executionSessions")?
        .get(session_ref)?
        .get("codexThreadId")?
        .as_str()
        .map(String::from)
}

fn save_thread_for_session(root: &Path, session_ref: &str, thread_id: &str) -> Result<(), String> {
    let mut cloud = read_cloud_json(root).unwrap_or_else(|| json!({}));
    if !cloud.get("executionSessions").is_some_and(|value| value.is_object()) {
        cloud["executionSessions"] = json!({});
    }
    cloud["executionSessions"][session_ref] = json!({ "codexThreadId": thread_id });
    save_cloud_json(root, &cloud)
}

fn set_readonly(path: &Path, readonly: bool) {
    if let Ok(metadata) = std::fs::metadata(path) {
        let mut permissions = metadata.permissions();
        permissions.set_readonly(readonly);
        let _ = std::fs::set_permissions(path, permissions);
    }
}

async fn sync_workspace(cfg: &CloudAgentConfig, token: &str, job: &AgentJob) -> Result<PathBuf, String> {
    let root = workspace_dir(&job.team_id, &job.workspace_id);
    let files = root.join("files");
    let cameo = root.join(".cameo");
    std::fs::create_dir_all(&files).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&cameo).map_err(|e| e.to_string())?;
    let existing_cloud = read_cloud_json(&root);
    let execution_sessions = existing_cloud
        .as_ref()
        .and_then(|value| value.get("executionSessions").cloned())
        .unwrap_or_else(|| json!({}));
    std::fs::write(
        cameo.join("cloud.json"),
        serde_json::to_vec_pretty(&json!({
            "teamId": job.team_id,
            "workspaceId": job.workspace_id,
            "syncedRevision": job.workspace.revision,
            "executionSessionId": job.execution_session_id,
            "agentSessionRef": job.agent_session_ref,
            "executionSessions": execution_sessions,
        }))
        .map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    std::fs::write(
        cameo.join("board.json"),
        serde_json::to_vec_pretty(&job.workspace.board_doc).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;

    for asset in &job.assets {
        let name = safe_local_filename(&asset.filename)?;
        let target = files.join(name);
        let url = format!(
            "{}/api/v1/agent/jobs/{}/assets/{}/download",
            cfg.api_base, job.id, asset.id
        );
        let bytes = crate::net::client()
            .get(url)
            .bearer_auth(token)
            .send()
            .await
            .map_err(|e| e.to_string())?
            .error_for_status()
            .map_err(|e| e.to_string())?
            .bytes()
            .await
            .map_err(|e| e.to_string())?;
        set_readonly(&target, false);
        std::fs::write(&target, bytes).map_err(|e| e.to_string())?;
        set_readonly(&target, true);
    }
    Ok(root)
}

fn build_prompt(job: &AgentJob, workspace_dir: &Path, overlays: &HashMap<String, PathBuf>) -> String {
    let mut prompt = String::new();
    prompt.push_str("You are executing a Cameo Cloud workspace turn.\n");
    prompt.push_str("Create new image files under the files/ directory in this workspace. Do not modify existing input files or .cameo.\n\n");
    if !job.references.is_empty() {
        prompt.push_str("Selected reference image(s). Read these files to see what the user is pointing at:\n");
        for reference in &job.references {
            if let Some(asset) = job.assets.iter().find(|asset| asset.id == reference.asset_id) {
                let path = workspace_dir.join("files").join(&asset.filename);
                match overlays.get(&reference.placement_id) {
                    Some(overlay) => prompt.push_str(&format!(
                        "- {} (selected placement: {}; marking overlay: {})\n",
                        path.display(),
                        reference.placement_id,
                        overlay.display()
                    )),
                    None => prompt.push_str(&format!(
                        "- {} (selected placement: {})\n",
                        path.display(),
                        reference.placement_id
                    )),
                }
            }
        }
        prompt.push('\n');
    } else if !job.assets.is_empty() {
        prompt.push_str("Available workspace assets:\n");
        for asset in &job.assets {
            let path = workspace_dir.join("files").join(&asset.filename);
            prompt.push_str(&format!(
                "- {} ({}, objectKey: {})\n",
                path.display(), asset.mime, asset.object_key
            ));
        }
        prompt.push('\n');
    }
    prompt.push_str(&job.prompt);
    prompt
}

fn render_reference_overlays(root: &Path, job: &AgentJob) -> Result<HashMap<String, PathBuf>, String> {
    let mut overlays = HashMap::new();
    if job.references.is_empty() {
        return Ok(overlays);
    }
    let Some(annotations) = job.workspace.board_doc.get("annotations").and_then(|value| value.as_array()) else {
        return Ok(overlays);
    };
    let Some(placements) = job.workspace.board_doc.get("placements").and_then(|value| value.as_array()) else {
        return Ok(overlays);
    };
    let Some(assets) = job.workspace.board_doc.get("assets").and_then(|value| value.as_array()) else {
        return Ok(overlays);
    };
    let out_dir = root.join(".cameo").join("tmp").join("overlays");
    std::fs::create_dir_all(&out_dir).map_err(|e| e.to_string())?;
    for reference in &job.references {
        let related: Vec<&Value> = annotations
            .iter()
            .filter(|annotation| {
                annotation
                    .get("placementId")
                    .and_then(|value| value.as_str())
                    .is_some_and(|id| id == reference.placement_id)
            })
            .collect();
        if related.is_empty() {
            continue;
        }
        let Some(placement) = placements.iter().find(|placement| {
            placement
                .get("id")
                .and_then(|value| value.as_str())
                .is_some_and(|id| id == reference.placement_id)
        }) else {
            continue;
        };
        let asset_id = placement
            .get("assetId")
            .and_then(|value| value.as_str())
            .unwrap_or(&reference.asset_id);
        let asset = assets.iter().find(|asset| {
            asset.get("id").and_then(|value| value.as_str()).is_some_and(|id| id == asset_id)
        });
        let width = asset
            .and_then(|asset| asset.get("width"))
            .and_then(|value| value.as_u64())
            .unwrap_or(512)
            .clamp(1, 4096) as u32;
        let height = asset
            .and_then(|asset| asset.get("height"))
            .and_then(|value| value.as_u64())
            .unwrap_or(512)
            .clamp(1, 4096) as u32;
        let mut image = image::RgbaImage::from_pixel(width, height, image::Rgba([0, 0, 0, 0]));
        for annotation in related {
            let Some(points) = annotation.get("points").and_then(|value| value.as_array()) else {
                continue;
            };
            if points.len() < 2 {
                continue;
            }
            let ax = points[0].get("x").and_then(|value| value.as_f64()).unwrap_or(0.0);
            let ay = points[0].get("y").and_then(|value| value.as_f64()).unwrap_or(0.0);
            let bx = points[1].get("x").and_then(|value| value.as_f64()).unwrap_or(ax);
            let by = points[1].get("y").and_then(|value| value.as_f64()).unwrap_or(ay);
            draw_rect_outline(&mut image, ax, ay, bx, by);
        }
        let path = out_dir.join(format!("{}.png", safe_local_filename(&reference.placement_id)?));
        image.save(&path).map_err(|e| e.to_string())?;
        overlays.insert(reference.placement_id.clone(), path);
    }
    Ok(overlays)
}

fn draw_rect_outline(image: &mut image::RgbaImage, ax: f64, ay: f64, bx: f64, by: f64) {
    let width = image.width().saturating_sub(1) as f64;
    let height = image.height().saturating_sub(1) as f64;
    let min_x = (ax.min(bx).clamp(0.0, 1.0) * width).round() as u32;
    let max_x = (ax.max(bx).clamp(0.0, 1.0) * width).round() as u32;
    let min_y = (ay.min(by).clamp(0.0, 1.0) * height).round() as u32;
    let max_y = (ay.max(by).clamp(0.0, 1.0) * height).round() as u32;
    let color = image::Rgba([229, 57, 53, 220]);
    for offset in 0..3 {
        let x0 = min_x.saturating_add(offset).min(max_x);
        let y0 = min_y.saturating_add(offset).min(max_y);
        let x1 = max_x.saturating_sub(offset).max(x0);
        let y1 = max_y.saturating_sub(offset).max(y0);
        for x in x0..=x1 {
            image.put_pixel(x, y0, color);
            image.put_pixel(x, y1, color);
        }
        for y in y0..=y1 {
            image.put_pixel(x0, y, color);
            image.put_pixel(x1, y, color);
        }
    }
}

async fn upload_outputs(
    cfg: &CloudAgentConfig,
    token: &str,
    job: &AgentJob,
    outputs: &[PathBuf],
) -> Result<Vec<Value>, String> {
    let mut committed = Vec::new();
    let parent_placement_id = job.references.first().map(|reference| reference.placement_id.clone());
    for path in outputs {
        let Some(filename) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
        let mime = mime_guess::from_path(path)
            .first_or_octet_stream()
            .essence_str()
            .to_string();
        if !mime.starts_with("image/") {
            continue;
        }
        let (width, height) = image_dimensions(path).unwrap_or((512, 512));
        let upload: UploadUrlResponse = api_json(
            &cfg.api_base,
            &format!("/api/v1/agent/jobs/{}/assets/upload-url", job.id),
            Some(token),
            "POST",
            Some(json!({
                "filename": filename,
                "mime": mime,
                "sizeBytes": bytes.len(),
                "width": width,
                "height": height,
                "hash": blake3::hash(&bytes).to_hex().to_string(),
            })),
        )
        .await?;
        let upload_url = upload
            .upload_url
            .clone()
            .unwrap_or_else(|| format!("{}{}", cfg.api_base, upload.fallback_upload_url.clone().unwrap_or_default()));
        let mut req = crate::net::client().put(upload_url).body(bytes.clone());
        if upload.upload_url.is_none() {
            req = req.bearer_auth(token);
        }
        let res = req.send().await.and_then(|response| response.error_for_status());
        if let Err(e) = res {
            if !upload.upload_url.is_some() {
                return Err(e.to_string());
            }
            if let Some(fallback) = &upload.fallback_upload_url {
                crate::net::client()
                    .put(format!("{}{}", cfg.api_base, fallback))
                    .bearer_auth(token)
                    .body(bytes)
                    .send()
                    .await
                    .map_err(|e| e.to_string())?
                    .error_for_status()
                    .map_err(|e| e.to_string())?;
            } else {
                return Err(e.to_string());
            }
        }
        committed.push(json!({ "assetId": upload.asset_id, "parentPlacementId": parent_placement_id }));
    }
    Ok(committed)
}

fn image_dimensions(path: &Path) -> Option<(u32, u32)> {
    let reader = image::ImageReader::open(path).ok()?.with_guessed_format().ok()?;
    reader.into_dimensions().ok()
}

fn media_files(root: &Path) -> Result<HashSet<PathBuf>, String> {
    let mut out = HashSet::new();
    collect_media(root, &mut out)?;
    Ok(out)
}

fn collect_media(dir: &Path, out: &mut HashSet<PathBuf>) -> Result<(), String> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.to_string()),
    };
    for entry in entries {
        let entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path();
        if path.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with('.')) {
            continue;
        }
        let ty = entry.file_type().map_err(|e| e.to_string())?;
        if ty.is_symlink() {
            continue;
        }
        if ty.is_dir() {
            collect_media(&path, out)?;
        } else if is_image_path(&path) {
            out.insert(path);
        }
    }
    Ok(())
}

fn is_image_path(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| matches!(e.to_ascii_lowercase().as_str(), "png" | "jpg" | "jpeg" | "webp"))
        .unwrap_or(false)
}

fn safe_local_filename(name: &str) -> Result<String, String> {
    let lower = name.to_ascii_lowercase();
    let reserved = [
        "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7",
        "com8", "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8",
        "lpt9",
    ];
    let stem = lower.split('.').next().unwrap_or("");
    if name.is_empty()
        || name.contains('/')
        || name.contains('\\')
        || name.contains(':')
        || name.contains('\0')
        || name.chars().any(|c| c.is_control())
        || name == "."
        || name == ".."
        || reserved.contains(&stem)
        || Path::new(name).is_absolute()
    {
        return Err("unsafe filename".into());
    }
    Ok(name.to_string())
}

async fn api_json<T: for<'de> Deserialize<'de>>(
    api_base: &str,
    path: &str,
    bearer: Option<&str>,
    method: &str,
    body: Option<Value>,
) -> Result<T, String> {
    let url = format!("{}{}", api_base.trim_end_matches('/'), path);
    let method = reqwest::Method::from_bytes(method.as_bytes()).map_err(|e| e.to_string())?;
    let mut req = crate::net::client().request(method, url);
    if let Some(token) = bearer {
        req = req.bearer_auth(token);
    }
    if let Some(body) = body {
        req = req.json(&body);
    }
    let res = req.send().await.map_err(|e| e.to_string())?;
    let status = res.status();
    let text = res.text().await.map_err(|e| e.to_string())?;
    if !status.is_success() {
        return Err(format!("cloud api {status}: {text}"));
    }
    serde_json::from_str(&text).map_err(|e| format!("bad cloud json: {e}; body={text}"))
}

struct CloudCodexRunner {
    child: Child,
    stdin: Arc<TokioMutex<ChildStdin>>,
    pending: Arc<TokioMutex<HashMap<u64, oneshot::Sender<Result<Value, String>>>>>,
    events: mpsc::UnboundedReceiver<Value>,
    next_id: u64,
}

impl CloudCodexRunner {
    async fn start(cwd: &Path) -> Result<Self, String> {
        let search_path = codex::build_augmented_path(true);
        let codex_path = which::which_in("codex", Some(&search_path), cwd)
            .map_err(|e| format!("codex not found in augmented PATH: {e}"))?;
        let mut cmd = tokio::process::Command::new(codex_path);
        crate::process::hide_tokio_console_window(&mut cmd);
        cmd.arg("app-server")
            .current_dir(cwd)
            .env("PATH", search_path)
            .env("PWD", cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(unix)]
        cmd.process_group(0);
        crate::proxy::apply_to_subprocess(&mut cmd, Some(&crate::config::load().proxy));
        let mut child = cmd.spawn().map_err(|e| format!("spawn codex app-server: {e}"))?;
        let stdin = Arc::new(TokioMutex::new(child.stdin.take().ok_or("codex has no stdin")?));
        let stdout = child.stdout.take().ok_or("codex has no stdout")?;
        let stderr = child.stderr.take().ok_or("codex has no stderr")?;
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                tracing::info!(module = "cloud_agent_codex", "stderr: {line}");
            }
        });
        let pending: Arc<TokioMutex<HashMap<u64, oneshot::Sender<Result<Value, String>>>>> =
            Arc::new(TokioMutex::new(HashMap::new()));
        let (tx, rx) = mpsc::unbounded_channel();
        let pending_reader = pending.clone();
        let stdin_reader = stdin.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let Ok(value) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                if let Some(id) = value.get("id").and_then(|v| v.as_u64()) {
                    if value.get("method").is_some() {
                        let response = json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "error": { "code": -32601, "message": "Cameo Cloud Agent does not support server-initiated JSON-RPC requests" },
                        });
                        if let Ok(line) = serde_json::to_string(&response) {
                            let mut stdin = stdin_reader.lock().await;
                            let _ = stdin.write_all(format!("{line}\n").as_bytes()).await;
                            let _ = stdin.flush().await;
                        }
                        continue;
                    }
                    if let Some(sender) = pending_reader.lock().await.remove(&id) {
                        let result = if let Some(error) = value.get("error") {
                            Err(error.to_string())
                        } else {
                            Ok(value.get("result").cloned().unwrap_or(Value::Null))
                        };
                        let _ = sender.send(result);
                    }
                } else {
                    let _ = tx.send(value);
                }
            }
        });
        let mut runner = Self {
            child,
            stdin,
            pending,
            events: rx,
            next_id: 1,
        };
        runner
            .call(
                "initialize",
                json!({
                    "clientInfo": { "name": "cameo-cloud-agent", "version": env!("CARGO_PKG_VERSION") },
                    "capabilities": {}
                }),
                15_000,
            )
            .await?;
        runner.notify("initialized", json!({})).await?;
        Ok(runner)
    }

    async fn run_turn(
        &mut self,
        cfg: &CloudAgentConfig,
        token: &str,
        job: &AgentJob,
        prompt: String,
        agent_session_ref: Option<String>,
    ) -> Result<String, String> {
        let root = workspace_dir(&job.team_id, &job.workspace_id);
        let session_ref = agent_session_ref.unwrap_or_else(|| format!("agent_sess_{}", uuid::Uuid::new_v4().simple()));
        let existing_thread_id = thread_id_for_session(&root, &session_ref);
        let thread_id = if let Some(thread_id) = existing_thread_id {
            let resumed = self
                .call(
                    "thread/resume",
                    json!({
                        "threadId": thread_id,
                        "approvalPolicy": "never",
                        "sandbox": "workspace-write",
                        "summary": "auto",
                        "personality": "friendly",
                    }),
                    20_000,
                )
                .await
                .is_ok();
            if resumed {
                thread_id
            } else {
                self.start_thread(job).await?
            }
        } else {
            self.start_thread(job).await?
        };
        save_thread_for_session(&root, &session_ref, &thread_id)?;
        let input = json!([{ "type": "text", "text": prompt, "text_elements": [] }]);
        let mut params = json!({
            "threadId": thread_id,
            "input": input,
            "summary": "auto",
            "effort": job.gen_settings.effort,
            "model": job.gen_settings.model,
            "serviceTier": job.gen_settings.service_tier,
            "personality": "friendly",
            "approvalPolicy": "never",
            "sandboxPolicy": { "type": "workspaceWrite" },
        });
        if job.gen_settings.service_tier.is_none() {
            params["serviceTier"] = Value::Null;
        }
        let start_res = self.call("turn/start", params, 20_000).await?;
        let turn_id = turn_id_of(&start_res);
        let mut seq = 0u64;
        let mut cancel_tick = tokio::time::interval(Duration::from_secs(2));
        loop {
            tokio::select! {
                event = self.events.recv() => {
                    let Some(event) = event else {
                        return Err("codex app-server stopped before turn completed".into());
                    };
                    let raw_kind = event
                        .get("method")
                        .and_then(|v| v.as_str())
                        .unwrap_or("notification")
                        .to_string();
                    let kind = cloud_event_kind(&raw_kind);
                    let _: Value = api_json(
                        &cfg.api_base,
                        &format!("/api/v1/agent/jobs/{}/events", job.id),
                        Some(token),
                        "POST",
                        Some(json!({ "seq": seq, "kind": kind, "payload": { "codexMethod": raw_kind, "event": event } })),
                    )
                    .await?;
                    seq += 1;
                    if raw_kind == "turn/completed" {
                        break;
                    }
                }
                _ = cancel_tick.tick() => {
                    if cancel_requested(cfg, token, &job.id).await? {
                        if let Some(turn_id) = turn_id.as_deref() {
                            let _ = self.call(
                                "turn/interrupt",
                                json!({ "threadId": thread_id, "turnId": turn_id }),
                                3_000,
                            ).await;
                        }
                        self.kill_tree();
                        return Err("cancelled".into());
                    }
                }
            }
        }
        self.kill_tree();
        Ok(session_ref)
    }

    async fn start_thread(&mut self, job: &AgentJob) -> Result<String, String> {
        let res = self
            .call(
                "thread/start",
                json!({
                    "cwd": workspace_dir(&job.team_id, &job.workspace_id),
                    "approvalPolicy": "never",
                    "sandbox": "workspace-write",
                    "summary": "auto",
                    "personality": "friendly",
                }),
                20_000,
            )
            .await?;
        res.get("thread")
            .and_then(|t| t.get("id"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .ok_or_else(|| format!("thread/start did not return thread id: {res}"))
    }

    async fn call(&mut self, method: &str, params: Value, timeout_ms: u64) -> Result<Value, String> {
        let id = self.next_id;
        self.next_id += 1;
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id, tx);
        self.write(json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }))
            .await?;
        tokio::time::timeout(Duration::from_millis(timeout_ms), rx)
            .await
            .map_err(|_| format!("rpc {method} timed out"))?
            .map_err(|_| format!("rpc {method} response channel closed"))?
    }

    async fn notify(&mut self, method: &str, params: Value) -> Result<(), String> {
        self.write(json!({ "jsonrpc": "2.0", "method": method, "params": params }))
            .await
    }

    async fn write(&self, value: Value) -> Result<(), String> {
        let mut stdin = self.stdin.lock().await;
        let line = serde_json::to_string(&value).map_err(|e| e.to_string())? + "\n";
        stdin.write_all(line.as_bytes()).await.map_err(|e| e.to_string())?;
        stdin.flush().await.map_err(|e| e.to_string())
    }

    fn kill_tree(&mut self) {
        if let Some(pid) = self.child.id() {
            kill_tree(pid, 15);
            let _ = self.child.start_kill();
        }
    }
}

impl Drop for CloudCodexRunner {
    fn drop(&mut self) {
        self.kill_tree();
    }
}

fn cloud_event_kind(raw_kind: &str) -> String {
    match raw_kind {
        "turn/started" => "cameo.turn.started".into(),
        "turn/completed" => "cameo.turn.completed".into(),
        "item/updated" => "cameo.item.updated".into(),
        "item/completed" => "cameo.item.completed".into(),
        "rate_limits/updated" => "cameo.rateLimits.updated".into(),
        other => format!("cameo.codex.{}", other.replace('/', ".")),
    }
}

fn turn_id_of(res: &Value) -> Option<String> {
    res.get("turn")
        .and_then(|turn| turn.get("id"))
        .or_else(|| res.get("turnId"))
        .and_then(|value| value.as_str())
        .map(String::from)
}

async fn cancel_requested(cfg: &CloudAgentConfig, token: &str, job_id: &str) -> Result<bool, String> {
    let manifest: ManifestResponse = api_json(
        &cfg.api_base,
        &format!("/api/v1/agent/jobs/{job_id}/manifest"),
        Some(token),
        "GET",
        None,
    )
    .await?;
    Ok(manifest.job.status == "cancelling")
}

#[cfg(unix)]
fn kill_tree(pid: u32, sig: i32) {
    use nix::sys::signal::{kill, Signal};
    use nix::unistd::Pid;
    if pid == 0 {
        return;
    }
    if let Ok(signal) = Signal::try_from(sig) {
        let _ = kill(Pid::from_raw(-(pid as i32)), signal);
    }
}

#[cfg(windows)]
fn kill_tree(pid: u32, _sig: i32) {
    if pid == 0 {
        return;
    }
    let mut cmd = std::process::Command::new("taskkill");
    crate::process::hide_console_window(&mut cmd);
    let _ = cmd
        .args(["/PID", &pid.to_string(), "/T", "/F"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
}
