//! The model registry: documented per-model limits, matched by id
//! prefix, driving the encoder's known-model gates.

/// One documented model's limits and capabilities.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ModelInfo {
    /// The documented input token limit.
    pub context_window_tokens: u32,
    /// The documented output token limit.
    pub max_output_tokens: u32,
    /// Whether the model carries a documented thinking request.
    pub thinking: bool,
    /// Whether the model accepts image input.
    pub multimodal: bool,
}

/// One registry row: the id prefix and the limits it stands for.
#[derive(Clone, Copy, Debug)]
pub struct ModelFamily {
    pub prefix: &'static str,
    pub info: ModelInfo,
}

const CHAT_CONTEXT_TOKENS: u32 = 1_048_576;
const CHAT_OUTPUT_TOKENS: u32 = 65_536;
const LEGACY_OUTPUT_TOKENS: u32 = 8_192;
const TTS_CONTEXT_TOKENS: u32 = 8_192;
const TTS_OUTPUT_TOKENS: u32 = 16_384;

const fn family(
    prefix: &'static str,
    context_window_tokens: u32,
    max_output_tokens: u32,
    thinking: bool,
    multimodal: bool,
) -> ModelFamily {
    ModelFamily {
        prefix,
        info: ModelInfo {
            context_window_tokens,
            max_output_tokens,
            thinking,
            multimodal,
        },
    }
}

/// The documented rows. The longest matching prefix wins, so the
/// text-only and lite variants outrank their family prefixes.
const FAMILIES: &[ModelFamily] = &[
    family(
        "gemini-2.0-flash",
        CHAT_CONTEXT_TOKENS,
        LEGACY_OUTPUT_TOKENS,
        false,
        true,
    ),
    family(
        "gemini-2.0-flash-lite",
        CHAT_CONTEXT_TOKENS,
        LEGACY_OUTPUT_TOKENS,
        false,
        true,
    ),
    family(
        "gemini-2.5-flash",
        CHAT_CONTEXT_TOKENS,
        CHAT_OUTPUT_TOKENS,
        true,
        true,
    ),
    family(
        "gemini-2.5-flash-preview-tts",
        TTS_CONTEXT_TOKENS,
        TTS_OUTPUT_TOKENS,
        false,
        false,
    ),
    family(
        "gemini-2.5-flash-lite",
        CHAT_CONTEXT_TOKENS,
        CHAT_OUTPUT_TOKENS,
        true,
        true,
    ),
    family(
        "gemini-2.5-pro",
        CHAT_CONTEXT_TOKENS,
        CHAT_OUTPUT_TOKENS,
        true,
        true,
    ),
    family(
        "gemini-3-flash",
        CHAT_CONTEXT_TOKENS,
        CHAT_OUTPUT_TOKENS,
        true,
        true,
    ),
    family(
        "gemini-3.1-flash-lite",
        CHAT_CONTEXT_TOKENS,
        CHAT_OUTPUT_TOKENS,
        true,
        true,
    ),
    family(
        "gemini-3.1-pro",
        CHAT_CONTEXT_TOKENS,
        CHAT_OUTPUT_TOKENS,
        true,
        true,
    ),
    family(
        "gemini-3.5-flash-lite",
        CHAT_CONTEXT_TOKENS,
        CHAT_OUTPUT_TOKENS,
        true,
        true,
    ),
    family(
        "gemini-3.6-flash",
        CHAT_CONTEXT_TOKENS,
        CHAT_OUTPUT_TOKENS,
        true,
        true,
    ),
    family(
        "gemini-3.8-flash",
        CHAT_CONTEXT_TOKENS,
        CHAT_OUTPUT_TOKENS,
        true,
        true,
    ),
    family(
        "gemini-3.8-flash-tts",
        TTS_CONTEXT_TOKENS,
        TTS_OUTPUT_TOKENS,
        false,
        false,
    ),
];

/// The model row an id resolves to, by longest matching prefix.
/// Unknown ids resolve to nothing and pass through untouched.
pub fn lookup(model_id: &str) -> Option<ModelInfo> {
    FAMILIES
        .iter()
        .filter(|row| model_id.starts_with(row.prefix))
        .max_by_key(|row| row.prefix.len())
        .map(|row| row.info)
}

/// Every documented row.
pub const fn families() -> &'static [ModelFamily] {
    FAMILIES
}
