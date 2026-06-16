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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CloudAgentConfig {
    pub api_base: String,
    pub agent_id: String,
    pub team_id: String,
    pub refresh_token: String,
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
    prompt: String,
    gen_settings: CloudGenSettings,
    workspace: WorkspaceManifest,
    agent_session_ref: Option<String>,
    assets: Vec<CloudAsset>,
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
    CloudAgentStatus {
        configured: cfg.is_some(),
        running,
        api_base: cfg.as_ref().map(|c| c.api_base.clone()),
        agent_id: cfg.as_ref().map(|c| c.agent_id.clone()),
        team_id: cfg.as_ref().map(|c| c.team_id.clone()),
        name: cfg.as_ref().map(|c| c.name.clone()),
        max_concurrent_sessions: cfg.as_ref().map(|c| c.max_concurrent_sessions).unwrap_or(6),
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
    let cfg = CloudAgentConfig {
        api_base,
        agent_id: res.agent_id,
        team_id: res.team_id,
        refresh_token: res.refresh_token,
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
    let handle = tokio::spawn(run_loop(app, cfg));
    *guard = Some(handle);
    Ok(())
}

pub async fn stop(state: Arc<CloudAgentState>) {
    if let Some(handle) = state.task.lock().await.take() {
        handle.abort();
    }
}

pub fn start_if_configured(app: AppHandle) {
    if load_config().is_none() {
        return;
    }
    let state = app.state::<Arc<CloudAgentState>>().inner().clone();
    tauri::async_runtime::spawn(async move {
        let _ = start(app, state).await;
    });
}

async fn run_loop(app: AppHandle, cfg: CloudAgentConfig) {
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
            heartbeat(&cfg, &token, 0).await?;
            let jobs: JobsResponse = api_json(&cfg.api_base, "/api/v1/agent/jobs", Some(&token), "GET", None).await?;
            for job in jobs.jobs {
                if let Err(e) = run_job(&app, &cfg, &token, &job.id).await {
                    emit(&app, "error", format!("job {} failed: {e}", job.id));
                }
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
    let res: TokenResponse = api_json(
        &cfg.api_base,
        "/api/v1/agents/token",
        None,
        "POST",
        Some(json!({ "agentId": cfg.agent_id, "refreshToken": cfg.refresh_token })),
    )
    .await?;
    Ok(res.access_token)
}

async fn heartbeat(cfg: &CloudAgentConfig, token: &str, active: u32) -> Result<(), String> {
    let _: Value = api_json(
        &cfg.api_base,
        "/api/v1/agent/heartbeat",
        Some(token),
        "POST",
        Some(json!({
            "maxConcurrentSessions": cfg.max_concurrent_sessions,
            "activeSessionCount": active,
            "activeJobs": [],
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

async fn run_job(app: &AppHandle, cfg: &CloudAgentConfig, token: &str, job_id: &str) -> Result<(), String> {
    emit(app, "job", format!("leasing {job_id}"));
    let leased: LeaseResponse = api_json(
        &cfg.api_base,
        &format!("/api/v1/agent/jobs/{job_id}/lease"),
        Some(token),
        "POST",
        None,
    )
    .await?;
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
    let workspace_dir = sync_workspace(cfg, token, &job).await?;
    set_job_status(cfg, token, job_id, "running", None).await?;
    let before = media_files(&workspace_dir)?;
    let mut runner = CloudCodexRunner::start(&workspace_dir).await?;
    let agent_session_ref = runner
        .run_turn(cfg, token, &job, build_prompt(&job, &workspace_dir), leased.job.agent_session_ref)
        .await?;
    set_job_status(cfg, token, job_id, "uploading", None).await?;
    let after = media_files(&workspace_dir)?;
    let outputs: Vec<PathBuf> = after.difference(&before).cloned().collect();
    let committed = upload_outputs(cfg, token, &job, &outputs).await?;
    if !committed.is_empty() {
        let _: Value = api_json(
            &cfg.api_base,
            &format!("/api/v1/agent/jobs/{job_id}/assets/commit"),
            Some(token),
            "POST",
            Some(json!({ "outputs": committed })),
        )
        .await?;
    }
    let _: Value = api_json(
        &cfg.api_base,
        &format!("/api/v1/agent/jobs/{job_id}/complete"),
        Some(token),
        "POST",
        Some(json!({ "status": "completed", "agentSessionRef": agent_session_ref })),
    )
    .await?;
    emit(app, "job", format!("completed {job_id}"));
    Ok(())
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

async fn sync_workspace(cfg: &CloudAgentConfig, token: &str, job: &AgentJob) -> Result<PathBuf, String> {
    let root = workspace_dir(&job.team_id, &job.workspace_id);
    let files = root.join("files");
    let cameo = root.join(".cameo");
    std::fs::create_dir_all(&files).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&cameo).map_err(|e| e.to_string())?;
    std::fs::write(
        cameo.join("cloud.json"),
        serde_json::to_vec_pretty(&json!({
            "teamId": job.team_id,
            "workspaceId": job.workspace_id,
            "syncedRevision": job.workspace.revision,
            "executionSessionId": job.execution_session_id,
            "agentSessionRef": job.agent_session_ref,
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
        if target.exists() {
            continue;
        }
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
        std::fs::write(target, bytes).map_err(|e| e.to_string())?;
    }
    Ok(root)
}

fn build_prompt(job: &AgentJob, workspace_dir: &Path) -> String {
    let mut prompt = String::new();
    prompt.push_str("You are executing a Cameo Cloud workspace turn.\n");
    prompt.push_str("Create or edit image files under the files/ directory in this workspace. Do not modify .cameo.\n\n");
    if !job.assets.is_empty() {
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

async fn upload_outputs(
    cfg: &CloudAgentConfig,
    token: &str,
    job: &AgentJob,
    outputs: &[PathBuf],
) -> Result<Vec<Value>, String> {
    let mut committed = Vec::new();
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
        let mut req = crate::net::client().put(upload_url).body(bytes);
        if upload.upload_url.is_none() {
            req = req.bearer_auth(token);
        }
        req.send()
            .await
            .map_err(|e| e.to_string())?
            .error_for_status()
            .map_err(|e| e.to_string())?;
        committed.push(json!({ "assetId": upload.asset_id }));
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
    if name.contains('/') || name.contains('\\') || name.contains('\0') || name == "." || name == ".." {
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
        let stdin = child.stdin.take().ok_or("codex has no stdin")?;
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
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let Ok(value) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                if let Some(id) = value.get("id").and_then(|v| v.as_u64()) {
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
            stdin: Arc::new(TokioMutex::new(stdin)),
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
        let thread_id = if let Some(thread_id) = agent_session_ref {
            match self
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
            {
                Ok(_) => thread_id,
                Err(_) => self.start_thread(job).await?,
            }
        } else {
            self.start_thread(job).await?
        };
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
        self.call("turn/start", params, 20_000).await?;
        let mut seq = 0u64;
        while let Some(event) = self.events.recv().await {
            let kind = event
                .get("method")
                .and_then(|v| v.as_str())
                .unwrap_or("notification")
                .to_string();
            let _: Value = api_json(
                &cfg.api_base,
                &format!("/api/v1/agent/jobs/{}/events", job.id),
                Some(token),
                "POST",
                Some(json!({ "seq": seq, "kind": kind, "payload": event })),
            )
            .await?;
            seq += 1;
            if kind == "turn/completed" {
                break;
            }
        }
        let _ = self.child.start_kill();
        Ok(thread_id)
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
}
