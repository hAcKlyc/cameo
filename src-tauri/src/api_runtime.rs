//! OpenAI-compatible image API runtime.
//!
//! This is the small direct-API branch for users who do not want to depend on a
//! local Codex login. It keeps Cameo's existing event stream, board placement,
//! lineage, and session timeline semantics, but it does not emulate Codex's
//! stateful agent loop, tool stream, or clarifying questions.

use crate::board::{self, BoardRegistry};
use crate::codex::{ModelInfo, SkillInputRef};
use crate::config::{self, ApiImageSettings};
use crate::model::{Asset, Origin, Placement};
use crate::runtime::{CodexEventEnvelope, UnifiedEvent, CODEX_EVENT};
use crate::{assets, session, storage};
use base64::Engine;
use parking_lot::Mutex;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tauri::{AppHandle, Emitter};

const API_TIMEOUT: Duration = Duration::from_secs(180);
const MAX_API_SKILL_CONTEXT_CHARS: usize = 12_000;
const MAX_API_SKILL_BODY_CHARS: usize = 9_000;

#[derive(Default)]
pub struct ApiRegistry {
    inner: Mutex<HashMap<String, Arc<ApiSession>>>,
}

impl ApiRegistry {
    fn get(&self, board_id: &str) -> Option<Arc<ApiSession>> {
        self.inner.lock().get(board_id).cloned()
    }

    fn insert(&self, board_id: String, session: Arc<ApiSession>) {
        self.inner.lock().insert(board_id, session);
    }

    fn remove(&self, board_id: &str) -> Option<Arc<ApiSession>> {
        self.inner.lock().remove(board_id)
    }
}

struct ApiSession {
    app: AppHandle,
    registry: Arc<BoardRegistry>,
    board_id: String,
    folder: PathBuf,
    active_session_id: Mutex<String>,
    next_turn: AtomicU64,
    in_flight: Mutex<Option<u64>>,
}

impl ApiSession {
    fn emit(&self, event: UnifiedEvent) {
        let env = CodexEventEnvelope {
            board_id: self.board_id.clone(),
            event,
        };
        let _ = self.app.emit(CODEX_EVENT, env);
    }

    fn begin_turn(&self) -> Result<u64, String> {
        let mut in_flight = self.in_flight.lock();
        if in_flight.is_some() {
            return Err(
                "API runtime is already generating. Stop it or wait for it to finish.".into(),
            );
        }
        let turn_id = self.next_turn.fetch_add(1, Ordering::SeqCst) + 1;
        *in_flight = Some(turn_id);
        Ok(turn_id)
    }

    fn is_current_turn(&self, turn_id: u64) -> bool {
        *self.in_flight.lock() == Some(turn_id)
    }

    fn finish_turn(&self, turn_id: u64) {
        let mut in_flight = self.in_flight.lock();
        if *in_flight == Some(turn_id) {
            *in_flight = None;
        }
    }

    fn active_session(&self) -> String {
        self.active_session_id.lock().clone()
    }

    fn set_active_session(&self, id: String) {
        *self.active_session_id.lock() = id;
    }
}

struct ResolvedRef {
    clean_rel: String,
    clean_abs: PathBuf,
    overlay_abs: Option<PathBuf>,
}

struct GeneratedImage {
    bytes: Vec<u8>,
    caption: Option<String>,
}

#[derive(Deserialize)]
struct ImagesResponse {
    data: Vec<ImageData>,
}

#[derive(Deserialize)]
struct ImageData {
    #[serde(default)]
    b64_json: Option<String>,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    revised_prompt: Option<String>,
}

pub async fn start_session(
    app: AppHandle,
    board_reg: Arc<BoardRegistry>,
    api_reg: Arc<ApiRegistry>,
    board_id: String,
) -> Result<String, String> {
    if let Some(existing) = api_reg.get(&board_id) {
        let active = existing.active_session();
        if !active.is_empty() {
            return Ok(active);
        }
    }

    let cfg = config::load();
    validate_api_settings(&cfg.api)?;
    let folder = board_reg.folder(&board_id).ok_or("unknown board")?;
    let legacy = storage::load_meta(&folder).thread_id.clone();
    let sessions = session::ensure_initial(&folder, legacy);
    let active = sessions.active_session_id.clone().unwrap_or_default();

    let inner = Arc::new(ApiSession {
        app,
        registry: board_reg,
        board_id: board_id.clone(),
        folder: folder.clone(),
        active_session_id: Mutex::new(active.clone()),
        next_turn: AtomicU64::new(0),
        in_flight: Mutex::new(None),
    });
    api_reg.insert(board_id.clone(), inner.clone());

    let mut meta = storage::load_meta(&folder);
    meta.runtime = Some("api".into());
    meta.active_session_id = Some(active.clone());
    storage::save_meta(&folder, &meta);

    inner.emit(UnifiedEvent::SessionInit {
        thread_id: active.clone(),
        model: cfg.api.model,
    });
    tracing::info!(
        module = "api-runtime",
        board = %board_id,
        session = %active,
        "API runtime ready"
    );
    Ok(active)
}

pub async fn send_message(
    api_reg: Arc<ApiRegistry>,
    board_id: String,
    text: String,
    source_placement_ids: Vec<String>,
    overlays: Vec<(String, String)>,
    skills: Vec<SkillInputRef>,
) -> Result<(), String> {
    let session = api_reg.get(&board_id).ok_or("session not started")?;
    let cfg = config::load();
    validate_api_settings(&cfg.api)?;

    let overlay_map: HashMap<String, String> = overlays.into_iter().collect();
    let overlay_paths: Vec<String> = overlay_map.values().cloned().collect();
    let refs = match resolve_refs(&session, &source_placement_ids, &overlay_map) {
        Ok(refs) => refs,
        Err(e) => {
            cleanup_overlay_paths(&session.folder, overlay_paths);
            return Err(e);
        }
    };
    let turn_id = match session.begin_turn() {
        Ok(id) => id,
        Err(e) => {
            cleanup_overlay_paths(&session.folder, overlay_paths);
            return Err(e);
        }
    };

    persist_user_record(
        &session.folder,
        &session.active_session(),
        &display_user_text(&text, &skills),
        &source_placement_ids,
    );
    let (placeholder_id, out_index) = start_generation(&session, &source_placement_ids);
    let prompt = build_api_prompt(&text, &refs, &skills)?;

    let result = generate_image(&cfg.api, &prompt, &refs).await;
    cleanup_overlay_paths(&session.folder, overlay_paths);

    if !session.is_current_turn(turn_id) {
        return Ok(());
    }

    match result {
        Ok(generated) => {
            match place_generated(
                &session,
                &generated.bytes,
                generated.caption.clone(),
                Some(placeholder_id.clone()),
                out_index,
                &source_placement_ids,
            ) {
                Ok((asset, placement)) => {
                    persist_assistant_image(
                        &session.folder,
                        &session.active_session(),
                        &placement.id,
                        generated.caption.as_deref(),
                    );
                    session.emit(UnifiedEvent::ImageGenerated {
                        asset,
                        placement,
                        caption: generated.caption,
                        placeholder_id: Some(placeholder_id),
                    });
                    session.emit(UnifiedEvent::TurnComplete {
                        status: "completed".into(),
                        error: None,
                    });
                    session.finish_turn(turn_id);
                    Ok(())
                }
                Err(e) => {
                    finish_with_error(&session, turn_id, &e);
                    Ok(())
                }
            }
        }
        Err(e) => {
            finish_with_error(&session, turn_id, &e);
            Ok(())
        }
    }
}

pub async fn interrupt_turn(api_reg: Arc<ApiRegistry>, board_id: String) -> Result<(), String> {
    let Some(session) = api_reg.get(&board_id) else {
        return Ok(());
    };
    let turn = { session.in_flight.lock().take() };
    if turn.is_some() {
        let msg = "API image request was cancelled.";
        persist_assistant_error(&session.folder, &session.active_session(), msg);
        session.emit(UnifiedEvent::TurnComplete {
            status: "aborted".into(),
            error: Some(msg.into()),
        });
    }
    Ok(())
}

pub async fn stop_session(api_reg: Arc<ApiRegistry>, board_id: &str) {
    if let Some(session) = api_reg.remove(board_id) {
        *session.in_flight.lock() = None;
    }
}

pub fn new_session(api_reg: Arc<ApiRegistry>, board_id: String) -> Result<String, String> {
    let session = api_reg.get(&board_id).ok_or("session not started")?;
    if session.in_flight.lock().is_some() {
        return Err("Cannot create a new session while the API runtime is working.".into());
    }
    let meta = session::new_session(&session.folder);
    session.set_active_session(meta.id.clone());
    let mut board_meta = storage::load_meta(&session.folder);
    board_meta.active_session_id = Some(meta.id.clone());
    storage::save_meta(&session.folder, &board_meta);
    Ok(meta.id)
}

pub fn switch_session(
    api_reg: Arc<ApiRegistry>,
    board_id: String,
    session_id: String,
) -> Result<(), String> {
    let session = api_reg.get(&board_id).ok_or("session not started")?;
    if session.in_flight.lock().is_some() {
        return Err("Cannot switch sessions while the API runtime is working.".into());
    }
    session::set_active(&session.folder, &session_id);
    session.set_active_session(session_id.clone());
    let mut meta = storage::load_meta(&session.folder);
    meta.active_session_id = Some(session_id);
    storage::save_meta(&session.folder, &meta);
    Ok(())
}

pub fn list_models() -> Vec<ModelInfo> {
    let model = config::load().api.model;
    vec![ModelInfo {
        id: model.clone(),
        display_name: model,
        default_reasoning_effort: Some("medium".into()),
        supported_efforts: vec!["medium".into()],
        service_tiers: Vec::new(),
        default_service_tier: None,
    }]
}

fn validate_api_settings(api: &ApiImageSettings) -> Result<(), String> {
    if api.base_url.trim().is_empty() {
        return Err("API base URL is required.".into());
    }
    if api.model.trim().is_empty() {
        return Err("API image model is required.".into());
    }
    Ok(())
}

fn resolve_refs(
    session: &Arc<ApiSession>,
    source_placement_ids: &[String],
    overlay_map: &HashMap<String, String>,
) -> Result<Vec<ResolvedRef>, String> {
    let Some(entry) = session.registry.get(&session.board_id) else {
        return Err("unknown board".into());
    };
    let doc = entry.doc.lock();
    Ok(source_placement_ids
        .iter()
        .filter_map(|pid| {
            let p = doc.placements.iter().find(|p| &p.id == pid)?;
            let a = doc.assets.iter().find(|a| a.id == p.asset_id)?;
            Some(ResolvedRef {
                clean_rel: a.path.clone(),
                clean_abs: entry.folder.join(&a.path),
                overlay_abs: overlay_map.get(pid).map(|rel| entry.folder.join(rel)),
            })
        })
        .collect())
}

fn start_generation(session: &Arc<ApiSession>, source_placement_ids: &[String]) -> (String, i64) {
    let out_index = 0;
    let placeholder_id = nanoid::nanoid!();
    let rect = {
        let Some(entry) = session.registry.get(&session.board_id) else {
            return (placeholder_id, out_index);
        };
        let doc = entry.doc.lock();
        let source_pair = source_placement_ids.first().and_then(|sid| {
            let p = doc.placements.iter().find(|p| &p.id == sid)?.clone();
            let a = doc.assets.iter().find(|a| a.id == p.asset_id)?.clone();
            Some((p, a))
        });
        board::placeholder_rect(source_pair.as_ref().map(|(p, a)| (p, a)), out_index, &doc)
    };
    session.emit(UnifiedEvent::GenerationStarted {
        placeholder_id: placeholder_id.clone(),
        x: rect.0,
        y: rect.1,
        w: rect.2,
        h: rect.3,
    });
    (placeholder_id, out_index)
}

fn build_api_prompt(
    text: &str,
    refs: &[ResolvedRef],
    skills: &[SkillInputRef],
) -> Result<String, String> {
    let mut cleaned = text.to_string();
    for r in refs {
        cleaned = cleaned.replace(&r.clean_rel, "attached image");
    }
    if !refs.is_empty() {
        let overlay_hint = if refs.iter().any(|r| r.overlay_abs.is_some()) {
            concat!(
                " Some attached images contain red numbered mark overlays;",
                " use those marks only as edit instructions",
                " and do not copy the red marks into the output."
            )
        } else {
            ""
        };
        cleaned = format!("Use the attached image(s) as visual reference.{overlay_hint}\n\n{cleaned}");
    }
    let skill_context = build_skill_context(skills)?;
    if skill_context.is_empty() {
        Ok(cleaned)
    } else if cleaned.trim().is_empty() {
        Ok(skill_context)
    } else {
        Ok(format!("{skill_context}\n\nUser request:\n{cleaned}"))
    }
}

fn display_user_text(text: &str, skills: &[SkillInputRef]) -> String {
    if skills.is_empty() {
        return text.to_string();
    }
    let skill_line = format!(
        "Skills: {}",
        skills
            .iter()
            .map(|s| s.name.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    );
    if text.trim().is_empty() {
        skill_line
    } else {
        format!("{text}\n\n{skill_line}")
    }
}

fn build_skill_context(skills: &[SkillInputRef]) -> Result<String, String> {
    if skills.is_empty() {
        return Ok(String::new());
    }
    let mut out = String::from(
        concat!(
            "Apply the following selected local Codex Skill instructions to this image generation request. ",
            "Because this request is executed through a direct image API, convert prompt-writing workflows ",
            "into final image-generation constraints. Generate the requested image directly; do not answer ",
            "with a prompt template, analysis, or checklist unless the user explicitly asks for text.\n"
        ),
    );
    for skill in skills {
        let body = std::fs::read_to_string(&skill.path)
            .map_err(|e| format!("read skill {} at {}: {e}", skill.name, skill.path))?;
        let compact = compact_skill_body(&body);
        out.push_str(&format!(
            "\n<skill name=\"{}\" path=\"{}\">\n{}\n</skill>\n",
            skill.name,
            skill.path,
            compact.trim()
        ));
        if out.len() >= MAX_API_SKILL_CONTEXT_CHARS {
            out.truncate(MAX_API_SKILL_CONTEXT_CHARS);
            out.push_str("\n...[skill context truncated]\n");
            break;
        }
    }
    Ok(out)
}

fn compact_skill_body(body: &str) -> String {
    let stripped = strip_frontmatter(body).trim();
    if stripped.len() <= MAX_API_SKILL_BODY_CHARS {
        return stripped.to_string();
    }

    let mut selected = String::new();
    for heading in [
        "## Goal",
        "## Workflow",
        "## Realism Hard Constraints",
        "## Variation System",
        "## Scene Presets",
        "## Prompt Template",
        "## Negative Prompt",
    ] {
        if let Some(section) = markdown_section(stripped, heading) {
            if !selected.is_empty() {
                selected.push_str("\n\n");
            }
            selected.push_str(section.trim());
        }
    }

    if selected.is_empty() {
        selected = stripped.chars().take(MAX_API_SKILL_BODY_CHARS).collect();
    }
    if selected.len() > MAX_API_SKILL_BODY_CHARS {
        selected.truncate(MAX_API_SKILL_BODY_CHARS);
        selected.push_str("\n...[skill body truncated]");
    }
    selected
}

fn strip_frontmatter(body: &str) -> &str {
    let Some(rest) = body.strip_prefix("---\n") else {
        return body;
    };
    match rest.find("\n---") {
        Some(end) => rest[end + 4..].trim_start_matches('\n'),
        None => body,
    }
}

fn markdown_section<'a>(body: &'a str, heading: &str) -> Option<&'a str> {
    let start = body.find(heading)?;
    let after = start + heading.len();
    let next = body[after..]
        .find("\n## ")
        .map(|offset| after + offset)
        .unwrap_or(body.len());
    Some(&body[start..next])
}

async fn generate_image(
    api: &ApiImageSettings,
    prompt: &str,
    refs: &[ResolvedRef],
) -> Result<GeneratedImage, String> {
    if prefers_responses_api(&api.model) {
        match call_responses_image(api, prompt, refs).await {
            Ok(image) => Ok(image),
            Err(responses_err) if image_endpoint_model(&api.model) => call_image_api(api, prompt, refs)
                .await
                .map_err(|image_err| {
                    format!(
                        "Responses image generation failed: {responses_err}; Image API fallback failed: {image_err}"
                    )
                }),
            Err(responses_err) => Err(format!(
                "Responses image generation failed: {responses_err}; Image API fallback skipped because configured model \"{}\" is not an image endpoint model.",
                api.model.trim()
            )),
        }
    } else {
        match call_image_api(api, prompt, refs).await {
            Ok(image) => Ok(image),
            Err(image_err) => call_responses_image(api, prompt, refs)
                .await
                .map_err(|responses_err| {
                    format!(
                        "Image API request failed: {image_err}; Responses fallback failed: {responses_err}"
                    )
                }),
        }
    }
}

fn prefers_responses_api(model: &str) -> bool {
    !image_endpoint_model(model)
}

fn image_endpoint_model(model: &str) -> bool {
    let model = model.trim().to_ascii_lowercase();
    model.starts_with("gpt-image") || model.starts_with("dall-e") || model.contains("image")
}

async fn call_image_api(
    api: &ApiImageSettings,
    prompt: &str,
    refs: &[ResolvedRef],
) -> Result<GeneratedImage, String> {
    if refs.is_empty() {
        call_generation(api, prompt).await
    } else {
        call_edit(api, prompt, refs).await
    }
}

async fn call_generation(api: &ApiImageSettings, prompt: &str) -> Result<GeneratedImage, String> {
    let body = json!({
        "model": api.model.trim(),
        "prompt": prompt,
        "n": 1,
        "size": api.size.trim(),
    });
    let resp = with_auth(
        api_client().post(endpoint(&api.base_url, "images/generations")),
        api,
    )
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("API request failed: {e}"))?;
    parse_image_response(resp).await
}

async fn call_edit(
    api: &ApiImageSettings,
    prompt: &str,
    refs: &[ResolvedRef],
) -> Result<GeneratedImage, String> {
    let image_paths = image_input_paths(refs);
    let field = if image_paths.len() > 1 { "image[]" } else { "image" };
    let mut form = reqwest::multipart::Form::new()
        .text("model", api.model.trim().to_string())
        .text("prompt", prompt.to_string())
        .text("n", "1".to_string())
        .text("size", api.size.trim().to_string());
    for path in image_paths {
        let bytes = std::fs::read(&path)
            .map_err(|e| format!("read reference image {}: {e}", path.display()))?;
        let mime = mime_guess::from_path(&path)
            .first_or_octet_stream()
            .to_string();
        let file_name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("image.png")
            .to_string();
        let part = reqwest::multipart::Part::bytes(bytes)
            .file_name(file_name)
            .mime_str(&mime)
            .map_err(|e| format!("reference image MIME rejected: {e}"))?;
        form = form.part(field, part);
    }
    let resp = with_auth(
        api_client().post(endpoint(&api.base_url, "images/edits")),
        api,
    )
        .multipart(form)
        .send()
        .await
        .map_err(|e| format!("API request failed: {e}"))?;
    parse_image_response(resp).await
}

async fn call_responses_image(
    api: &ApiImageSettings,
    prompt: &str,
    refs: &[ResolvedRef],
) -> Result<GeneratedImage, String> {
    let image_paths = image_input_paths(refs);
    let input = if image_paths.is_empty() {
        json!(prompt)
    } else {
        let mut content = vec![json!({
            "type": "input_text",
            "text": prompt,
        })];
        for path in image_paths {
            content.push(responses_image_part(&path)?);
        }
        json!([{
            "role": "user",
            "content": content,
        }])
    };
    let mut tool = json!({ "type": "image_generation" });
    if !api.size.trim().is_empty() {
        tool["size"] = json!(api.size.trim());
    }
    let body = json!({
        "model": api.model.trim(),
        "input": input,
        "tools": [tool],
        "tool_choice": { "type": "image_generation" },
        "store": false,
    });
    let url = endpoint(&api.base_url, "responses");
    let mut last_err = None;
    let resp = loop {
        match with_auth(api_client().post(&url), api).json(&body).send().await {
            Ok(resp) => break resp,
            Err(e) => {
                let message = format!("Responses API request failed: {e}");
                if last_err.is_none() && (e.is_connect() || e.is_timeout() || e.is_request()) {
                    last_err = Some(message);
                    tokio::time::sleep(Duration::from_millis(800)).await;
                    continue;
                }
                return Err(message);
            }
        }
    };
    parse_responses_image_response(resp).await
}

fn responses_image_part(path: &Path) -> Result<Value, String> {
    let bytes =
        std::fs::read(path).map_err(|e| format!("read response image {}: {e}", path.display()))?;
    let mime = mime_guess::from_path(path)
        .first_or_octet_stream()
        .to_string();
    let b64 = base64::engine::general_purpose::STANDARD.encode(bytes);
    Ok(json!({
        "type": "input_image",
        "image_url": format!("data:{mime};base64,{b64}"),
    }))
}

fn image_input_paths(refs: &[ResolvedRef]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for r in refs {
        out.push(r.clean_abs.clone());
        if let Some(overlay) = &r.overlay_abs {
            out.push(overlay.clone());
        }
    }
    out
}

fn api_client() -> reqwest::Client {
    let cfg = config::load();
    let mut b = reqwest::Client::builder().timeout(API_TIMEOUT);
    if cfg.proxy.enabled {
        match cfg.proxy.proxy_url() {
            Ok(url) => match reqwest::Proxy::all(&url) {
                Ok(proxy) => b = b.proxy(proxy),
                Err(e) => {
                    tracing::warn!(module = "api-runtime", "proxy rejected ({e}); going direct")
                }
            },
            Err(e) => tracing::warn!(module = "api-runtime", "proxy invalid ({e}); going direct"),
        }
    } else {
        b = b.no_proxy();
    }
    b.build().unwrap_or_else(|e| {
        tracing::warn!(module = "api-runtime", "client build failed ({e}); using default client");
        reqwest::Client::new()
    })
}

fn with_auth(builder: reqwest::RequestBuilder, api: &ApiImageSettings) -> reqwest::RequestBuilder {
    let builder = builder.header(reqwest::header::ACCEPT, "application/json");
    if api.api_key.trim().is_empty() {
        builder
    } else {
        builder.bearer_auth(api.api_key.trim())
    }
}

fn endpoint(base_url: &str, path: &str) -> String {
    format!(
        "{}/{}",
        base_url.trim().trim_end_matches('/'),
        path.trim_start_matches('/')
    )
}

async fn parse_image_response(resp: reqwest::Response) -> Result<GeneratedImage, String> {
    let status = resp.status();
    let body = resp
        .text()
        .await
        .map_err(|e| format!("read API response: {e}"))?;
    if !status.is_success() {
        return Err(format!("API returned {status}: {}", api_error_message(&body)));
    }
    let parsed: ImagesResponse =
        serde_json::from_str(&body).map_err(|e| format!("parse API response: {e}"))?;
    let Some(first) = parsed.data.into_iter().next() else {
        return Err("API response did not include an image.".into());
    };
    if let Some(b64) = first.b64_json.filter(|s| !s.is_empty()) {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(b64)
            .map_err(|e| format!("decode API image: {e}"))?;
        return Ok(GeneratedImage {
            bytes,
            caption: first.revised_prompt,
        });
    }
    if let Some(url) = first.url.filter(|s| !s.is_empty()) {
        let bytes = read_image_url(&url).await?;
        return Ok(GeneratedImage {
            bytes,
            caption: first.revised_prompt,
        });
    }
    Err("API image item had neither b64_json nor url.".into())
}

async fn parse_responses_image_response(resp: reqwest::Response) -> Result<GeneratedImage, String> {
    let status = resp.status();
    let body = resp
        .text()
        .await
        .map_err(|e| format!("read Responses API response: {e}"))?;
    if !status.is_success() {
        return Err(format!("Responses API returned {status}: {}", api_error_message(&body)));
    }
    let parsed: Value =
        serde_json::from_str(&body).map_err(|e| format!("parse Responses API response: {e}"))?;
    let Some((b64, caption)) = find_response_image(&parsed) else {
        return Err("Responses API response did not include an image_generation_call result.".into());
    };
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(b64)
        .map_err(|e| format!("decode Responses API image: {e}"))?;
    Ok(GeneratedImage { bytes, caption })
}

fn find_response_image(value: &Value) -> Option<(String, Option<String>)> {
    match value {
        Value::Object(map) => {
            if map.get("type").and_then(Value::as_str) == Some("image_generation_call") {
                if let Some(result) = map.get("result").and_then(Value::as_str) {
                    let caption = map
                        .get("revised_prompt")
                        .and_then(Value::as_str)
                        .map(str::to_string);
                    return Some((result.to_string(), caption));
                }
            }
            map.values().find_map(find_response_image)
        }
        Value::Array(items) => items.iter().find_map(find_response_image),
        _ => None,
    }
}

fn api_error_message(body: &str) -> String {
    let Ok(v) = serde_json::from_str::<Value>(body) else {
        return body.chars().take(600).collect();
    };
    v.get("error")
        .and_then(|e| e.get("message"))
        .and_then(|m| m.as_str())
        .or_else(|| v.get("message").and_then(|m| m.as_str()))
        .unwrap_or(body)
        .chars()
        .take(600)
        .collect()
}

async fn read_image_url(url: &str) -> Result<Vec<u8>, String> {
    if let Some((_, data)) = url.split_once(',').filter(|(head, _)| head.starts_with("data:")) {
        return base64::engine::general_purpose::STANDARD
            .decode(data)
            .map_err(|e| format!("decode data URL image: {e}"));
    }
    let resp = api_client()
        .get(url)
        .send()
        .await
        .map_err(|e| format!("download API image: {e}"))?;
    let status = resp.status();
    if !status.is_success() {
        return Err(format!("download API image returned {status}"));
    }
    resp.bytes()
        .await
        .map(|b| b.to_vec())
        .map_err(|e| format!("read API image bytes: {e}"))
}

fn place_generated(
    session: &Arc<ApiSession>,
    bytes: &[u8],
    caption: Option<String>,
    placeholder_id: Option<String>,
    out_index: i64,
    source_placement_ids: &[String],
) -> Result<(Asset, Placement), String> {
    let assets_snapshot = match session.registry.get(&session.board_id) {
        Some(entry) => entry.doc.lock().assets.clone(),
        None => return Err("unknown board".into()),
    };
    let ext = image_ext(bytes);
    let asset = assets::import_bytes(
        &session.folder,
        bytes,
        ext,
        "gen",
        Origin::Generated,
        &assets_snapshot,
    )
    .map_err(|e| format!("write generated image: {e}"))?;

    let Some(entry) = session.registry.get(&session.board_id) else {
        return Err("unknown board".into());
    };
    let save_guard = entry.save.lock();
    let (placement, doc_clone) = {
        let mut doc = entry.doc.lock();
        let source_pair = source_placement_ids.first().and_then(|sid| {
            let p = doc.placements.iter().find(|p| &p.id == sid)?.clone();
            let a = doc.assets.iter().find(|a| a.id == p.asset_id)?.clone();
            Some((p, a))
        });
        let placement = board::make_derived_placement(
            &asset,
            source_pair.as_ref().map(|(p, a)| (p, a)),
            out_index,
            &doc,
        );
        if !doc.assets.iter().any(|a| a.id == asset.id) {
            doc.assets.push(asset.clone());
        }
        doc.placements.push(placement.clone());
        (placement, doc.clone())
    };
    storage::save_board_doc(&session.folder, &doc_clone)
        .map_err(|e| format!("save board after generation: {e}"))?;
    drop(save_guard);
    tracing::info!(
        module = "api-runtime",
        placement = %placement.id,
        placeholder = ?placeholder_id,
        caption = ?caption,
        "API image generated"
    );
    Ok((asset, placement))
}

fn image_ext(bytes: &[u8]) -> &'static str {
    match image::guess_format(bytes) {
        Ok(image::ImageFormat::Png) => "png",
        Ok(image::ImageFormat::Jpeg) => "jpg",
        Ok(image::ImageFormat::WebP) => "webp",
        Ok(image::ImageFormat::Gif) => "gif",
        Ok(image::ImageFormat::Bmp) => "bmp",
        Ok(image::ImageFormat::Tiff) => "tiff",
        Ok(image::ImageFormat::Avif) => "avif",
        _ => "png",
    }
}

fn persist_user_record(folder: &Path, session_id: &str, text: &str, refs: &[String]) {
    if session_id.is_empty() {
        return;
    }
    let msg = json!({
        "id": nanoid::nanoid!(12),
        "role": "user",
        "text": text,
        "refs": refs,
    });
    session::append_message(folder, session_id, &msg);
}

fn persist_assistant_image(
    folder: &Path,
    session_id: &str,
    placement_id: &str,
    caption: Option<&str>,
) {
    if session_id.is_empty() {
        return;
    }
    let msg = json!({
        "id": nanoid::nanoid!(12),
        "role": "assistant",
        "blocks": [{
            "type": "image",
            "placementId": placement_id,
            "caption": caption,
            "status": "done",
        }],
        "status": "done",
    });
    session::append_message(folder, session_id, &msg);
}

fn persist_assistant_error(folder: &Path, session_id: &str, error: &str) {
    if session_id.is_empty() {
        return;
    }
    let msg = json!({
        "id": nanoid::nanoid!(12),
        "role": "assistant",
        "blocks": [],
        "status": "error",
        "error": error,
    });
    session::append_message(folder, session_id, &msg);
}

fn finish_with_error(session: &Arc<ApiSession>, turn_id: u64, message: &str) {
    persist_assistant_error(&session.folder, &session.active_session(), message);
    session.emit(UnifiedEvent::Log {
        level: "error".into(),
        message: message.into(),
    });
    session.emit(UnifiedEvent::TurnComplete {
        status: "error".into(),
        error: Some(message.into()),
    });
    session.finish_turn(turn_id);
}

fn is_overlay_temp_path(path: &str) -> bool {
    let p = Path::new(path);
    p.components().count() == 1 && path.starts_with(".overlay-") && path.ends_with(".png")
}

fn cleanup_overlay_paths(folder: &Path, overlays: Vec<String>) {
    for rel in overlays {
        if !is_overlay_temp_path(&rel) {
            tracing::warn!(
                module = "api-runtime",
                overlay = %rel,
                "skip unsafe overlay cleanup path"
            );
            continue;
        }
        if let Err(e) = std::fs::remove_file(folder.join(&rel)) {
            if e.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!(
                    module = "api-runtime",
                    overlay = %rel,
                    "overlay cleanup failed: {e}"
                );
            }
        }
    }
}
