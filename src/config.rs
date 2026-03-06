use std::env;

#[derive(Debug, Clone)]
pub struct LlmConfig {
    pub provider: String,
    pub api_key: String,
    pub api_base_url: String,
    pub model: String,
}

impl LlmConfig {
    pub fn from_env() -> anyhow::Result<Self> {
        let provider = env::var("LLM_PROVIDER").unwrap_or_else(|_| "minimax".to_string());

        let api_key =
            env::var("LLM_API_KEY").map_err(|_| anyhow::anyhow!("LLM_API_KEY is required"))?;

        let api_base_url = env::var("LLM_API_BASE_URL").unwrap_or_else(|_| {
            // Default to MiniMax OpenAI-compatible base URL (China).
            "https://api.minimaxi.com/v1".to_string()
        });

        let model = env::var("LLM_MODEL").unwrap_or_else(|_| "MiniMax-M2.5".to_string());

        Ok(Self {
            provider,
            api_key,
            api_base_url,
            model,
        })
    }
}
