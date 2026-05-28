//! Runtime-agnostic event stream. The Codex adapter (codex.rs) translates Codex
//! JSON-RPC notifications into `UnifiedEvent`s; everything above the adapter
//! consumes only these, keeping the runtime swappable (Codex now, others later).
//!
//! This is the deliberate swap boundary — there is one concrete runtime today,
//! so we keep the abstraction at the event shape rather than a premature trait.

use crate::model::{Asset, Placement};
use serde::Serialize;

/// One step in the agent's plan (turn/plan/updated). status ∈ pending |
/// inProgress | completed.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanStep {
    pub step: String,
    pub status: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum UnifiedEvent {
    /// Thread established (start or resume).
    SessionInit { thread_id: String, model: String },
    /// Assistant text token.
    TextDelta { text: String },
    TextStop,
    /// Reasoning / plan stream.
    ThinkingStart,
    ThinkingDelta { text: String },
    ThinkingStop,
    /// Generic tool lifecycle (Bash/Edit/Read/ImageGeneration/…) for the chat log.
    /// `detail` is the first-level gray subtitle (the command / file / query).
    ToolStart { tool_use_id: String, tool_name: String, detail: Option<String> },
    ToolStop { tool_use_id: String },
    ToolResult { tool_use_id: String, content: String },
    /// A generation just started — show a loading placeholder at the predicted
    /// landing spot (right of source). Replaced by `ImageGenerated`.
    GenerationStarted {
        placeholder_id: String,
        x: f64,
        y: f64,
        w: f64,
        h: f64,
    },
    /// The payload Cameo lives on: a generated image, already minted as an Asset
    /// and placed (right of source) on the board. `placeholder_id` (if any) is
    /// the loading placeholder to remove.
    ImageGenerated {
        asset: Asset,
        placement: Placement,
        caption: Option<String>,
        placeholder_id: Option<String>,
    },
    /// Codex asked the client to approve something (auto-accepted in v1; shown).
    PermissionRequest { request_id: u64, summary: String },
    /// Turn finished. `status` ∈ completed / aborted / error.
    TurnComplete { status: String, error: Option<String> },
    Usage { input_tokens: u64, output_tokens: u64 },
    /// The agent's plan/todo for the turn (turn/plan/updated).
    PlanUpdated { explanation: Option<String>, steps: Vec<PlanStep> },
    /// Subscription rate-limit usage (account/rateLimits/updated). `used_percent`
    /// + `resets_at` are the **primary (5-hour rolling)** window; `secondary_*`
    /// are the **weekly** window. `reached` indicates which one (if any) the
    /// user has actually hit (`"primary"` / `"secondary"`).
    RateLimits {
        used_percent: f64,
        resets_at: Option<f64>,
        secondary_used_percent: Option<f64>,
        secondary_resets_at: Option<f64>,
        reached: Option<String>,
    },
    Status { state: String },
    /// Structured runtime transport state derived inside the runtime adapter.
    TransportStatus {
        phase: String,
        attempt: Option<u64>,
        max: Option<u64>,
        message: String,
    },
    /// Fatal runtime failure. Recoverable runtime diagnostics are `Log` events;
    /// turn state should normally settle through `TurnComplete`.
    Error { message: String },
    /// Process exited / session ended.
    SessionComplete { ok: bool, message: String },
    Log { level: String, message: String },
}

/// Envelope emitted on the Tauri event channel `codex-event`, tagging which
/// Board the event belongs to.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexEventEnvelope {
    pub board_id: String,
    pub event: UnifiedEvent,
}

pub const CODEX_EVENT: &str = "codex-event";
