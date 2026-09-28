//! In-process GGUF inference via Goose's [`LocalInferenceProvider`], plus the memory-aware model
//! scheduler. Metal is automatic on macOS; the Jetson needs `--features cuda`.

/// Whether this build has CUDA; `cuda` is this crate's feature, invisible to `cfg!` elsewhere.
pub const CUDA_ENABLED: bool = cfg!(feature = "cuda");

pub mod scheduler;
pub mod tool_caller;
pub use scheduler::{
    NoopScheduler, ResourceAwareModelScheduler, JETSON_TOTAL_RAM_MB, LLM_BUDGET_MB,
};
pub use tool_caller::ToolCallerEngine;

use anyhow::Result;
use async_trait::async_trait;
use goose::providers::base::Provider as GooseProvider;
use goose::providers::local_inference::LocalInferenceProvider;
use goose_providers::model::ModelConfig;
use pond_adapters_goose::provider_adapter::GooseProviderAdapter;
use pond_core::models::domain::message::ChatMessage;
use pond_core::models::ports::provider::LlmProvider;
use std::sync::Arc;

/// Default GGUF: a 3B Q4_K_M (~2.5 GB at runtime) leaves headroom on an 8 GB Orin Nano.
pub const DEFAULT_MODEL: &str = "bartowski/Llama-3.2-3B-Instruct-GGUF:Q4_K_M";

/// `LlmProvider` over in-process GGUF; [`GooseProviderAdapter`] does the message conversion.
pub struct LocalInferenceLlmAdapter {
    inner: GooseProviderAdapter,
}

impl LocalInferenceLlmAdapter {
    pub const DEFAULT_MODEL: &'static str = DEFAULT_MODEL;

    /// `model_id`: HuggingFace `repo:QUANT` or a local path. Weights load on first `complete()`.
    pub async fn new(model_id: &str) -> Result<Self> {
        let model_config = ModelConfig::new(model_id);

        Self::apply_model_settings(model_id);

        tracing::info!(
            "initialising LocalInferenceProvider for model: {}",
            model_id
        );
        goose::providers::local_inference::configure_local_inference();
        let provider = LocalInferenceProvider::from_env().await?;

        Ok(Self {
            inner: GooseProviderAdapter::new(
                Arc::new(provider) as Arc<dyn GooseProvider>,
                model_config,
            ),
        })
    }

    /// Like `new`, but points Goose's registry at `$data_dir/models/gguf/` instead of Goose's own
    /// models dir. `model_id` is a `repo:QUANT` id or a `.gguf` file already in that dir.
    pub async fn new_with_data_dir(model_id: &str, data_dir: &std::path::Path) -> Result<Self> {
        use goose::providers::local_inference::local_model_registry::{
            get_registry, model_id_from_repo, LocalModelEntry, LocalModelStorage, ModelSettings,
        };

        let gguf_dir = data_dir.join("models").join("gguf");

        // The catalog stores bare stems but files are `{stem}.gguf`: normalise to the filename.
        let owned_with_ext;
        let model_id =
            if !model_id.contains('/') && !model_id.contains(':') && !model_id.ends_with(".gguf") {
                let candidate = gguf_dir.join(format!("{}.gguf", model_id));
                if candidate.exists() {
                    owned_with_ext = format!("{}.gguf", model_id);
                    owned_with_ext.as_str()
                } else {
                    model_id // not a local stem — fall through to HF path
                }
            } else {
                model_id
            };

        // ── Raw filename (e.g. "gemma-4-E2B-it-Q4_K_M.gguf") ────────────────
        if model_id.ends_with(".gguf") && !model_id.contains(':') {
            let path = std::path::Path::new(model_id);
            let filename = path
                .file_name()
                .map(|f| f.to_string_lossy().into_owned())
                .unwrap_or_else(|| model_id.to_string());

            // Stable synthetic registry key = stem (strip ".gguf").
            let stem = filename.trim_end_matches(".gguf").to_string();

            let local_path = if path.is_absolute() {
                path.to_path_buf()
            } else {
                gguf_dir.join(&filename)
            };

            let (tool_mode, thinking) = Self::registration_settings(&local_path);
            {
                match get_registry().lock() {
                    Ok(mut registry) => {
                        if !registry.has_model(&stem) {
                            let mut settings = ModelSettings::default();
                            settings.tool_calling = tool_mode;
                            settings.enable_thinking = thinking;
                            let entry = LocalModelEntry {
                                id: stem.clone(),
                                repo_id: format!("local/{}", stem),
                                filename: filename.clone(),
                                quantization: String::new(),
                                local_path,
                                source_url: String::new(),
                                backend_id: None,
                                storage: LocalModelStorage::ManualPath,
                                settings,
                                size_bytes: 0,
                                mmproj_path: None,
                                mmproj_source_url: None,
                                mmproj_size_bytes: 0,
                                mmproj_checked: false,
                                shard_files: vec![],
                            };
                            if let Err(e) = registry.add_model(entry) {
                                tracing::warn!("Could not register GGUF model '{}': {}", stem, e);
                            }
                        } else if let Some(entry) = registry.get_model(&stem) {
                            let mut s = entry.settings.clone();
                            if s.tool_calling != tool_mode || s.enable_thinking != thinking {
                                s.tool_calling = tool_mode;
                                s.enable_thinking = thinking;
                                let _ = registry.update_model_settings(&stem, s);
                            }
                        }
                    }
                    Err(e) => tracing::warn!("GGUF registry lock poisoned: {}", e),
                }
            }

            return Self::new(&stem).await;
        }

        // ── HuggingFace format ("repo_id:quantization") ───────────────────────
        let (repo_id, quantization) = model_id.rsplit_once(':').unwrap_or((model_id, "Q4_K_M"));

        let id = model_id_from_repo(repo_id, quantization);

        let model_name = repo_id.split('/').last().unwrap_or(repo_id);
        let base_name = model_name.strip_suffix("-GGUF").unwrap_or(model_name);
        let filename = format!("{}-{}.gguf", base_name, quantization);

        let local_path = gguf_dir.join(&filename);
        let source_url = format!(
            "https://huggingface.co/{}/resolve/main/{}",
            repo_id, filename
        );

        // The registry lock must be released before `Self::new()`, which locks it again.
        let (tool_mode, thinking) = Self::registration_settings(&local_path);
        {
            match get_registry().lock() {
                Ok(mut registry) => {
                    if !registry.has_model(&id) {
                        let mut settings = ModelSettings::default();
                        settings.tool_calling = tool_mode;
                        settings.enable_thinking = thinking;
                        let entry = LocalModelEntry {
                            id: id.clone(),
                            repo_id: repo_id.to_string(),
                            filename: filename.clone(),
                            quantization: quantization.to_string(),
                            local_path,
                            source_url,
                            backend_id: None,
                            storage: LocalModelStorage::ManualPath,
                            settings,
                            size_bytes: 0,
                            mmproj_path: None,
                            mmproj_source_url: None,
                            mmproj_size_bytes: 0,
                            mmproj_checked: false,
                            shard_files: vec![],
                        };
                        if let Err(e) = registry.add_model(entry) {
                            tracing::warn!("Could not register GGUF model '{}': {}", id, e);
                        }
                    } else if let Some(entry) = registry.get_model(&id) {
                        let mut s = entry.settings.clone();
                        if s.tool_calling != tool_mode || s.enable_thinking != thinking {
                            s.tool_calling = tool_mode;
                            s.enable_thinking = thinking;
                            let _ = registry.update_model_settings(&id, s);
                        }
                    }
                }
                Err(e) => tracing::warn!("GGUF registry lock poisoned: {}", e),
            }
        }

        Self::new(model_id).await
    }

    // Speculative decoding was taken out of the llama.cpp engine on 2026-09-24 (goose 743649d98),
    // so this is commented out rather than deleted; restore it if it returns.
    // /// The drafter id to hand the engine, or `None` to decode without speculation.
    // ///
    // /// `None` whenever the speculation switch in Settings is off: every call site stamps the
    // /// whole settings block, so a drafter decided here would switch speculation back on behind
    // /// the household's back at the next adapter build.
    // ///
    // /// Checks the file, not just the row: a registry entry whose weights have
    // /// been deleted would otherwise fail inside context creation on the next
    // /// turn, which reads as the engine breaking rather than as a missing file.
    // fn registered_drafter(model_id: &str) -> Option<String> {
    //     use goose::providers::local_inference::local_model_registry::get_registry;
    //     use pond_core::models::domain::drafter::{drafter_for, speculation_enabled};
    //
    //     if !speculation_enabled() {
    //         return None;
    //     }
    //     let spec = drafter_for(model_id)?;
    //     let registry = get_registry().lock().ok()?;
    //     let entry = registry.get_model(spec.id)?;
    //     entry.local_path.exists().then(|| spec.id.to_string())
    // }
    /// Stamp the model registry so llama-cpp-2 picks up device settings at load time.
    ///
    /// A device profile lets a non-CUDA build take the Jetson branch on purpose: otherwise
    /// `device_budget::device_window`, the arithmetic that can OOM the board, has no caller
    /// off-device.
    fn apply_model_settings(model_id: &str) {
        #[cfg(feature = "cuda")]
        Self::apply_jetson_settings(model_id);

        #[cfg(not(feature = "cuda"))]
        if pond_core::models::domain::device_profile::stamping_device_model_settings() {
            Self::apply_jetson_settings(model_id);
        } else {
            Self::apply_platform_settings(model_id);
        }
    }

    /// Non-CUDA settings. Without them `n_gpu_layers` is `None` and Apple Silicon runs on the CPU.
    #[cfg(not(feature = "cuda"))]
    fn apply_platform_settings(model_id: &str) {
        use goose::providers::local_inference::local_model_registry::{
            get_registry, ModelSettings, ToolCallingMode,
        };

        // An unreadable file leaves goose its own judgement (`Auto`).
        let probe = get_registry()
            .lock()
            .ok()
            .and_then(|reg| reg.get_model(model_id).map(|e| e.local_path.clone()))
            .and_then(|p| Self::probe_model(&p));
        let (tools, thinking) = match &probe {
            Some(p) => Self::tool_and_thinking_for(p),
            None => (ToolCallingMode::Auto, true),
        };
        if let Some(p) = &probe {
            tracing::info!(
                model = model_id,
                tools = ?p.tools,
                thinking = ?p.thinking,
                "model capabilities read from its chat template"
            );
        }

        let settings = ModelSettings {
            // Full offload: unified memory needs no CPU/GPU split.
            n_gpu_layers: Some(99),
            // Goose's `estimate_max_context_for_memory()` sizes it from free RAM.
            context_size: None,
            // Batch 512 is optimal for Metal prefill throughput.
            n_batch: Some(512),
            // Flash attention reduces KV-cache memory by ~40%.
            flash_attention: Some(true),
            // Unified memory — mlock is unnecessary and can cause issues.
            use_mlock: false,
            // From the model's own chat template; see `tool_and_thinking_for`.
            tool_calling: tools,
            enable_thinking: thinking,
            // Threads: llama.cpp's auto-detect suits Apple Silicon.
            ..Default::default()
        };

        match get_registry().lock() {
            Ok(mut registry) => {
                let applied = Self::stamp_every_row_naming(&mut registry, model_id, &settings);
                if applied.is_empty() {
                    tracing::debug!(
                        "Platform settings not applied to '{}' (model not yet registered)",
                        model_id
                    );
                } else {
                    tracing::info!(
                        rows = ?applied,
                        "{} settings applied to model '{}' (n_gpu_layers=99, ctx=dynamic, flash_attn=true)",
                        // On aarch64 Linux this means a build without `cuda`: inference is on
                        // the CPU, and saying "Metal" would hide the bad build.
                        if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
                            "CPU-ONLY (built without the cuda feature) —"
                        } else {
                            "Metal/platform"
                        },
                        model_id
                    );
                }
            }
            Err(e) => {
                tracing::warn!(
                    "Could not acquire model registry lock for platform settings: {}",
                    e
                );
            }
        }
    }

    /// Tool mode and thinking flag for a GGUF, from its own chat template. Always re-stamps, so an
    /// entry once persisted as `ForceNative` can't keep a mode its template can't honour.
    fn registration_settings(
        path: &std::path::Path,
    ) -> (
        goose::providers::local_inference::local_model_registry::ToolCallingMode,
        bool,
    ) {
        use goose::providers::local_inference::local_model_registry::ToolCallingMode;
        match Self::probe_model(path) {
            Some(probe) => Self::tool_and_thinking_for(&probe),
            None => (ToolCallingMode::Auto, true),
        }
    }

    /// Registry settings from a [`ModelProbe`]; a template without `tools` gets `ForceEmulated`.
    #[cfg_attr(not(feature = "cuda"), allow(dead_code))]
    fn tool_and_thinking_for(
        probe: &pond_core::models::domain::model_probe::ModelProbe,
    ) -> (
        goose::providers::local_inference::local_model_registry::ToolCallingMode,
        bool,
    ) {
        use goose::providers::local_inference::local_model_registry::ToolCallingMode;
        use pond_core::models::domain::model_probe::{Thinking, ToolSupport};

        let tools = match probe.tools {
            ToolSupport::Native => ToolCallingMode::ForceNative,
            ToolSupport::Absent => ToolCallingMode::ForceEmulated,
            ToolSupport::Unknown => ToolCallingMode::Auto,
        };
        let thinking = matches!(
            probe.thinking,
            Thinking::Gated { .. } | Thinking::Always { .. }
        );
        (tools, thinking)
    }

    /// Probe a GGUF's metadata; reaching `tokenizer.chat_template` (3.8-15 MB in) takes ~40 ms.
    #[cfg_attr(not(feature = "cuda"), allow(dead_code))]
    fn probe_model(
        path: &std::path::Path,
    ) -> Option<pond_core::models::domain::model_probe::ModelProbe> {
        use pond_core::models::domain::gguf::parse_gguf_file;
        use pond_core::models::domain::model_probe::ModelProbe;
        parse_gguf_file(path).map(|info| ModelProbe::from_gguf(&info))
    }

    /// Stamp Jetson settings into the registry; failures leave defaults. Not fail-closed on fit:
    /// past the budget `n_gpu_layers = 99` silently spills to CPU.
    fn apply_jetson_settings(model_id: &str) {
        use goose::providers::local_inference::local_model_registry::{
            get_registry, ModelSettings, ToolCallingMode,
        };

        // Size the context to THIS model, through pond-core's one derivation. The goose adapter
        // asks the same function whether picture support fits beside the model, so the window
        // stamped here and that answer cannot come from different inputs: the RESOLVED file's
        // length and header slope, never a registry row by name. A file that cannot be read is
        // charged as the largest model we ship, so the first load is conservative, not fatal.
        let gguf = get_registry().lock().ok().and_then(|reg| {
            reg.get_model(model_id)
                .map(|e| Self::resolved(&e.local_path))
        });
        let sizing =
            pond_core::models::domain::device_budget::device_window(gguf.as_deref(), model_id);
        let context_size = sizing.window;
        // The window above still charges this model's drafter (see `device_budget`), although
        // the engine no longer loads one: that keeps the Orin's windows at the values they were
        // measured at. Uncharging it is a separate, device-measured change.
        // Speculative decoding was taken out of the llama.cpp engine on 2026-09-24 (goose
        // 743649d98), so this is commented out rather than deleted; restore it if it returns.
        // let draft_model = Self::registered_drafter(model_id);
        tracing::info!(
            model = model_id,
            model_mb = sizing.model_bytes / (1024 * 1024),
            weights_known = sizing.weights_known,
            kv_kib_per_token = sizing
                .kv_kib_per_token
                .map_or("fallback".to_string(), |k| k.to_string()),
            context_size,
            drafter_mb = sizing.drafter_bytes / (1024 * 1024),
            encoder_mb = sizing.encoder_bytes / (1024 * 1024),
            // speculation = draft_model.is_some(),
            "Jetson context sized to fit this model's KV cache in the LLM budget"
        );

        let probe = get_registry()
            .lock()
            .ok()
            .and_then(|reg| reg.get_model(model_id).map(|e| e.local_path.clone()))
            .and_then(|p| Self::probe_model(&p));
        let (tools, thinking) = match &probe {
            Some(p) => Self::tool_and_thinking_for(p),
            None => (ToolCallingMode::Auto, true),
        };
        if let Some(p) = &probe {
            tracing::info!(
                model = model_id,
                tools = ?p.tools,
                thinking = ?p.thinking,
                "model capabilities read from its chat template"
            );
        }

        let jetson_settings = ModelSettings {
            // Full offload: unified memory, one 8 GB pool for CPU and GPU.
            n_gpu_layers: Some(99),
            // Turn 1 alone is ~3,250 tokens; KV buffers must stay below NvMap's ~586 MiB wall.
            context_size: Some(context_size),
            // 512 saturates the Ampere SMs in prefill within the 68 GB/s memory bandwidth.
            n_batch: Some(512),
            // 4 of the A78AE's 6 cores; 2 stay free for the OS, audio and GIAP services.
            n_threads: Some(4),
            // Halves KV memory on Ampere, and llama.cpp requires it for a quantised `type_v`.
            flash_attention: Some(true),
            // q8_0 KV: ~half of f16's memory, quality-neutral on this board; q4_0 is 6.4x noisier.
            type_k: Some("q8_0".to_string()),
            type_v: Some("q8_0".to_string()),
            // Compute buffer 522 -> 129 MiB versus the 512 default, at no measurable speed cost.
            n_ubatch: Some(128),
            // On unified memory mlock makes every GPU access page-fault.
            use_mlock: false,
            // From the model's own template; see `tool_and_thinking_for`.
            tool_calling: tools,
            enable_thinking: thinking,
            // Speculative decoding was taken out of the llama.cpp engine on 2026-09-24 (goose
            // 743649d98), so this is commented out rather than deleted; restore it if it returns.
            // // Speculative decoding, when the switch in Settings is on AND this
            // // model's drafter is registered AND its weights are still on disk.
            // // Re-decided on every provider build
            // // rather than configured once: `update_model_settings` below
            // // replaces the whole block, so a `draft_model` set by hand in
            // // registry.json is erased here anyway. Deciding it from the file
            // // system each time is what makes that safe -- a deleted drafter
            // // stops being referenced instead of failing the next context
            // // creation, and a newly downloaded one is picked up without a
            // // restart.
            // draft_model,
            // `enable_thinking` is set above from the template, not inherited.
            ..Default::default()
        };

        match get_registry().lock() {
            Ok(mut registry) => {
                let applied =
                    Self::stamp_every_row_naming(&mut registry, model_id, &jetson_settings);
                if applied.is_empty() {
                    tracing::debug!("Jetson settings applied to no row for '{}'", model_id);
                } else {
                    tracing::info!(
                        rows = ?applied,
                        "Applied Jetson Orin Nano CUDA settings"
                    );
                }
            }
            Err(e) => {
                tracing::warn!(
                    "Could not acquire model registry lock for Jetson settings: {}",
                    e
                );
            }
        }
    }

    /// Stamp `settings` on every registry row naming the same GGUF as `model_id`, returning the
    /// ids that took it.
    ///
    /// Every row, not only the id we were handed. One file is registered under more than one id
    /// -- the spelling in settings (`gemma-4-E2B-it-qat-UD-Q4_K_XL`) and the canonical stem
    /// `register_gguf_model` returns (`gemma-4-E2B-it-qat`) -- and they share one engine slot.
    /// Stamping only the id passed in put the whole tuning block on a row the engine never read:
    /// measured on the Orin, the row it did read carried context_size, flash_attention, type_k,
    /// type_v, n_batch and n_ubatch all None, so the pond ran with an f16 KV cache and the
    /// default 2048/512 batch instead of q8_0 and 512/128 -- roughly 700 MB of footprint on a
    /// 7.6 GB board. On the Mac the same split decided whether the drafter loaded by which
    /// caller happened to cold-load the slot first.
    ///
    /// Matching on the resolved path rather than on a name rule: the spellings are produced by
    /// two different canonicalisers in two crates, and a rule that tried to reproduce either
    /// would be one more thing to keep in step.
    ///
    /// Each row keeps its own copy of the picture-support fields (`mmproj_size_bytes`,
    /// `vision_capable`): the goose adapter's encoder stamp owns them, and a block that zeroed
    /// them at every build left the settings copy disagreeing with the row it sits on.
    fn stamp_every_row_naming(
        registry: &mut goose::providers::local_inference::local_model_registry::LocalModelRegistry,
        model_id: &str,
        settings: &goose::providers::local_inference::local_model_registry::ModelSettings,
    ) -> Vec<String> {
        let target = registry
            .get_model(model_id)
            .map(|e| Self::resolved(&e.local_path));
        let rows: Vec<(String, u64, bool)> = match &target {
            Some(path) => registry
                .list_models()
                .iter()
                .filter(|e| Self::resolved(&e.local_path) == *path)
                .map(|e| {
                    (
                        e.id.clone(),
                        e.settings.mmproj_size_bytes,
                        e.settings.vision_capable,
                    )
                })
                .collect(),
            None => vec![(model_id.to_string(), 0, false)],
        };
        let mut applied = Vec::new();
        for (id, mmproj_size_bytes, vision_capable) in rows {
            let mut row_settings = settings.clone();
            row_settings.mmproj_size_bytes = mmproj_size_bytes;
            row_settings.vision_capable = vision_capable;
            match registry.update_model_settings(&id, row_settings) {
                Ok(()) => applied.push(id),
                Err(e) => tracing::debug!(
                    "settings not applied to '{}' (model not yet registered): {}",
                    id,
                    e
                ),
            }
        }
        applied
    }

    /// Follow symlinks so rows naming one GGUF (a `models/gguf/` link or its blob) compare equal.
    fn resolved(p: &std::path::Path) -> std::path::PathBuf {
        std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
    }
}

/// Strip reasoning preambles: everything up to Gemma 4's last `<channel|>`, else `<thought>`
/// or `<think>` blocks (Qwen3, DeepSeek-R1, QwQ).
fn strip_thinking_tokens(text: &str) -> String {
    const CHANNEL_CLOSE: &str = "<channel|>";
    if let Some(pos) = text.rfind(CHANNEL_CLOSE) {
        return text[pos + CHANNEL_CLOSE.len()..].trim().to_string();
    }

    if text.contains("<thought>") {
        let mut out = String::with_capacity(text.len());
        let mut rest = text;
        loop {
            if let Some(start) = rest.find("<thought>") {
                out.push_str(&rest[..start]);
                if let Some(end) = rest[start..].find("</thought>") {
                    rest = &rest[start + end + "</thought>".len()..];
                } else {
                    break; // unclosed — discard tail
                }
            } else {
                out.push_str(rest);
                break;
            }
        }
        let trimmed = out.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
        return String::new();
    }

    if text.contains("<think>") {
        let mut out = String::with_capacity(text.len());
        let mut rest = text;
        loop {
            if let Some(start) = rest.find("<think>") {
                out.push_str(&rest[..start]);
                if let Some(end) = rest[start..].find("</think>") {
                    rest = &rest[start + end + "</think>".len()..];
                } else {
                    // Unclosed <think> — discard the rest
                    break;
                }
            } else {
                out.push_str(rest);
                break;
            }
        }
        let trimmed = out.trim().to_string();
        if !trimmed.is_empty() {
            return trimmed;
        }
    }

    text.to_string()
}

#[async_trait]
impl LlmProvider for LocalInferenceLlmAdapter {
    fn capabilities(&self) -> pond_core::models::domain::model_capabilities::ModelCapabilities {
        let name = self.inner.model_name();
        pond_core::models::domain::model_capabilities::ModelCapabilities::from_model_name(&name)
    }

    async fn complete(&self, system: &str, messages: Vec<ChatMessage>) -> Result<ChatMessage> {
        let mut msg = self.inner.complete(system, messages).await?;
        msg.content = strip_thinking_tokens(&msg.content);
        Ok(msg)
    }

    fn model_name(&self) -> String {
        self.inner.model_name()
    }
}

impl LocalInferenceLlmAdapter {
    /// `complete()` without `strip_thinking_tokens()`, for diagnostics.
    pub async fn raw_complete(
        &self,
        system: &str,
        messages: Vec<ChatMessage>,
    ) -> Result<ChatMessage> {
        self.inner.complete(system, messages).await
    }
}

// ── Unit tests (no model weights required) ────────────────────────────────────

#[cfg(test)]
mod tests {

    /// Run with `GIAP_TEST_GGUF_DIR` set to a `models/gguf` dir. The decisions must differ across
    /// files: a probe giving one answer for everything would look like it works.
    #[test]
    #[ignore = "needs real GGUF files; set GIAP_TEST_GGUF_DIR"]
    fn probe_model_reads_real_files_and_separates_them() {
        use goose::providers::local_inference::local_model_registry::ToolCallingMode;

        let Ok(dir) = std::env::var("GIAP_TEST_GGUF_DIR") else {
            eprintln!("GIAP_TEST_GGUF_DIR unset");
            return;
        };
        let mut modes = std::collections::BTreeMap::new();
        for entry in std::fs::read_dir(&dir).expect("dir") {
            let path = entry.expect("entry").path();
            if path.extension().and_then(|e| e.to_str()) != Some("gguf") {
                continue;
            }
            let (mode, thinking) = match LocalInferenceLlmAdapter::probe_model(&path) {
                Some(p) => LocalInferenceLlmAdapter::tool_and_thinking_for(&p),
                None => (ToolCallingMode::Auto, true),
            };
            eprintln!(
                "{:<44} {:?} thinking={}",
                path.file_name().unwrap().to_string_lossy(),
                mode,
                thinking
            );
            *modes.entry(format!("{mode:?}")).or_insert(0) += 1;
        }
        assert!(!modes.is_empty(), "no GGUF files under {dir}");
        assert!(
            modes.len() > 1,
            "every model resolved to the same tool mode ({modes:?}); a probe that cannot \
             tell them apart is the blanket ForceNative with extra steps"
        );
        assert!(
            modes.contains_key("ForceEmulated"),
            "expected at least one model whose template carries no `tools` variable; \
             got {modes:?}"
        );
    }

    #[test]
    fn a_tool_using_model_keeps_native_calling() {
        use goose::providers::local_inference::local_model_registry::ToolCallingMode;
        use pond_core::models::domain::model_probe::{ModelProbe, Thinking, ToolSupport};

        let gemma = ModelProbe {
            tools: ToolSupport::Native,
            thinking: Thinking::Gated {
                marker: "<|think|>".into(),
            },
            context_window_tokens: Some(131072),
            architecture: Some("gemma4".into()),
        };
        let (tools, thinking) = LocalInferenceLlmAdapter::tool_and_thinking_for(&gemma);
        assert_eq!(tools, ToolCallingMode::ForceNative);
        assert!(
            thinking,
            "a gated thinker must still get true -- this wiring must not change what \
             a currently-reasoning model does"
        );
    }

    /// DeepSeek-R1-Distill's template takes no `tools` variable.
    #[test]
    fn a_model_whose_template_cannot_carry_tools_is_not_forced_native() {
        use goose::providers::local_inference::local_model_registry::ToolCallingMode;
        use pond_core::models::domain::model_probe::{ModelProbe, Thinking, ToolSupport};

        let deepseek = ModelProbe {
            tools: ToolSupport::Absent,
            thinking: Thinking::Always {
                marker: "<think>".into(),
            },
            context_window_tokens: Some(131072),
            architecture: Some("qwen2".into()),
        };
        let (tools, thinking) = LocalInferenceLlmAdapter::tool_and_thinking_for(&deepseek);
        assert_eq!(
            tools,
            ToolCallingMode::ForceEmulated,
            "forcing native on a template with no `tools` variable renders declarations \
             nowhere at all"
        );
        assert!(
            thinking,
            "it reasons unconditionally; there is no flag to clear"
        );
    }

    #[test]
    fn an_unreadable_model_defers_rather_than_forcing() {
        use goose::providers::local_inference::local_model_registry::ToolCallingMode;
        use pond_core::models::domain::model_probe::{ModelProbe, Thinking, ToolSupport};

        let unknown = ModelProbe {
            tools: ToolSupport::Unknown,
            thinking: Thinking::Unknown,
            context_window_tokens: None,
            architecture: None,
        };
        let (tools, thinking) = LocalInferenceLlmAdapter::tool_and_thinking_for(&unknown);
        assert_eq!(tools, ToolCallingMode::Auto);
        assert!(!thinking, "nothing said it reasons");
    }

    #[test]
    fn a_model_with_no_reasoning_markers_does_not_get_the_flag() {
        use pond_core::models::domain::model_probe::{ModelProbe, Thinking, ToolSupport};

        let plain = ModelProbe {
            tools: ToolSupport::Native,
            thinking: Thinking::Absent,
            context_window_tokens: Some(32768),
            architecture: Some("gemma3".into()),
        };
        let (_, thinking) = LocalInferenceLlmAdapter::tool_and_thinking_for(&plain);
        assert!(!thinking);
    }

    use super::*;

    #[test]
    fn default_model_constant_is_set() {
        assert!(!DEFAULT_MODEL.is_empty());
    }

    #[test]
    fn default_model_has_huggingface_format() {
        // Expected: "org/repo-GGUF:QUANTIZATION"
        assert!(
            DEFAULT_MODEL.contains('/'),
            "DEFAULT_MODEL should be a HuggingFace repo path: {DEFAULT_MODEL}"
        );
        assert!(
            DEFAULT_MODEL.contains(':'),
            "DEFAULT_MODEL should have a quantization suffix (':'): {DEFAULT_MODEL}"
        );
    }

    #[test]
    fn associated_constant_matches_module_constant() {
        assert_eq!(
            LocalInferenceLlmAdapter::DEFAULT_MODEL,
            DEFAULT_MODEL,
            "associated constant must re-export the same value"
        );
    }

    // ── data_dir path-construction logic (no model weights required) ──────────

    /// Replicates `new_with_data_dir`'s filename logic: calling it would download weights.
    #[test]
    fn data_dir_filename_derivation_strips_gguf_suffix() {
        let model_id = "bartowski/Llama-3.2-3B-Instruct-GGUF:Q4_K_M";
        let (repo_id, quantization) = model_id.rsplit_once(':').unwrap();
        let model_name = repo_id.split('/').last().unwrap();
        let base_name = model_name.strip_suffix("-GGUF").unwrap_or(model_name);
        let filename = format!("{}-{}.gguf", base_name, quantization);

        assert_eq!(filename, "Llama-3.2-3B-Instruct-Q4_K_M.gguf");
    }

    #[test]
    fn data_dir_filename_without_gguf_suffix_kept_as_is() {
        let model_id = "bartowski/SomeModel:Q8_0";
        let (repo_id, quantization) = model_id.rsplit_once(':').unwrap();
        let model_name = repo_id.split('/').last().unwrap();
        let base_name = model_name.strip_suffix("-GGUF").unwrap_or(model_name);
        let filename = format!("{}-{}.gguf", base_name, quantization);

        assert_eq!(filename, "SomeModel-Q8_0.gguf");
    }

    #[test]
    fn rows_naming_one_file_through_a_symlink_compare_equal() {
        let tmp = tempfile::tempdir().unwrap();
        let blob = tmp.path().join("blobs").join("deadbeef");
        std::fs::create_dir_all(blob.parent().unwrap()).unwrap();
        std::fs::write(&blob, b"GGUF").unwrap();
        let link = tmp.path().join("model.gguf");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&blob, &link).unwrap();
        #[cfg(not(unix))]
        std::fs::copy(&blob, &link).unwrap();

        assert_eq!(
            LocalInferenceLlmAdapter::resolved(&link),
            LocalInferenceLlmAdapter::resolved(&blob),
            "a models/gguf symlink and its hf_cache blob are the same file"
        );
    }

    /// A row whose weights were deleted must still match itself to stay in stamping.
    #[test]
    fn a_missing_path_still_compares_with_itself() {
        let p = std::path::Path::new("/nowhere/at/all/model.gguf");
        assert_eq!(
            LocalInferenceLlmAdapter::resolved(p),
            LocalInferenceLlmAdapter::resolved(p)
        );
    }

    #[test]
    fn data_dir_gguf_path_is_under_models_gguf() {
        let data_dir = std::path::Path::new("/home/user/.giap");
        let filename = "Llama-3.2-3B-Instruct-Q4_K_M.gguf";
        let gguf_dir = data_dir.join("models").join("gguf");
        let local_path = gguf_dir.join(filename);

        assert_eq!(
            local_path.to_string_lossy(),
            "/home/user/.giap/models/gguf/Llama-3.2-3B-Instruct-Q4_K_M.gguf"
        );
    }

    #[test]
    fn data_dir_source_url_is_huggingface_resolve() {
        let repo_id = "bartowski/Llama-3.2-3B-Instruct-GGUF";
        let filename = "Llama-3.2-3B-Instruct-Q4_K_M.gguf";
        let source_url = format!(
            "https://huggingface.co/{}/resolve/main/{}",
            repo_id, filename
        );

        assert!(source_url.starts_with("https://huggingface.co/"));
        assert!(source_url.contains("/resolve/main/"));
        assert!(source_url.ends_with(filename));
    }

    #[test]
    fn strip_thinking_tokens_removes_gemma4_preamble() {
        let raw = "<|channel>thought Some reasoning here.<channel|>Hello! I am Goose.";
        assert_eq!(strip_thinking_tokens(raw), "Hello! I am Goose.");
    }

    #[test]
    fn strip_thinking_tokens_no_tag_returns_original() {
        let raw = "Hello! I am Goose.";
        assert_eq!(strip_thinking_tokens(raw), "Hello! I am Goose.");
    }

    #[test]
    fn strip_thinking_tokens_multiline_thinking() {
        let raw = "<|channel>thought\nStep 1.\nStep 2.\n<channel|>The answer is 4.";
        assert_eq!(strip_thinking_tokens(raw), "The answer is 4.");
    }

    #[test]
    fn strip_thinking_tokens_uses_last_close_tag() {
        let raw = "<|channel>thought Step 1.<channel|>intermediate<channel|>Final answer.";
        assert_eq!(strip_thinking_tokens(raw), "Final answer.");
    }

    #[test]
    fn strip_thinking_tokens_channel_close_at_end_returns_empty() {
        let raw = "<|channel>thought reasoning here<channel|>";
        assert_eq!(strip_thinking_tokens(raw), "");
    }

    #[test]
    fn strip_thinking_tokens_removes_thought_tags() {
        let raw = "<thought>internal reasoning</thought>Hello!";
        assert_eq!(strip_thinking_tokens(raw), "Hello!");
    }

    #[test]
    fn strip_thinking_tokens_thought_only_returns_empty() {
        let raw = "<thought>only reasoning</thought>";
        assert_eq!(strip_thinking_tokens(raw), "");
    }

    #[test]
    fn model_id_rsplit_fallback_uses_q4_k_m() {
        let model_id = "some-model-without-quant";
        let (_repo_id, quantization) = model_id.rsplit_once(':').unwrap_or((model_id, "Q4_K_M"));
        assert_eq!(quantization, "Q4_K_M");
    }

    /// Mirrors the Jetson `ModelSettings` literal; only hardware can check the values.
    #[test]
    fn the_jetson_settings_block_still_type_checks_off_device() {
        use goose::providers::local_inference::local_model_registry::{
            ModelSettings, ToolCallingMode,
        };

        let settings = ModelSettings {
            n_gpu_layers: Some(99),
            context_size: Some(16384),
            n_batch: Some(512),
            n_threads: Some(4),
            flash_attention: Some(true),
            type_k: Some("q8_0".to_string()),
            type_v: Some("q8_0".to_string()),
            n_ubatch: Some(128),
            use_mlock: false,
            tool_calling: ToolCallingMode::ForceNative,
            enable_thinking: true,
            ..Default::default()
        };

        assert_eq!(settings.n_ubatch, Some(128));
        assert_eq!(settings.type_k.as_deref(), Some("q8_0"));
        assert_eq!(settings.type_v.as_deref(), Some("q8_0"));
        // llama.cpp refuses a quantised V cache without flash attention.
        assert_eq!(settings.flash_attention, Some(true));
    }
}
