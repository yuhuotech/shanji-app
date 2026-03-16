use crate::config::RewriteConfig;
use crate::error::{AppError, Result};
use serde::{Deserialize, Serialize};
use std::time::Duration;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LlmProvider {
    pub id: String,
    pub name: String,
    pub base_url: String,
    pub model: String,
}

#[derive(Debug, Clone)]
pub struct LlmConfig {
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    pub system_prompt: String,
    pub max_tokens: u32,
    pub temperature: f32,
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            base_url: "https://api.openai.com/v1".to_string(),
            api_key: String::new(),
            model: "gpt-4o-mini".to_string(),
            system_prompt: "请将以下语音输入文本整理为适合直接输入或粘贴的最终文本：修正明显识别错误，去除语气词、口吃和重复表达，补全自然标点，保留原意，不要无端扩写。只输出整理后的文本。".to_string(),
            max_tokens: 2048,
            temperature: 0.7,
        }
    }
}

#[derive(Debug, Serialize)]
struct ChatRequest {
    model: String,
    messages: Vec<Message>,
    max_tokens: u32,
    temperature: f32,
    stream: bool,
}

#[derive(Debug, Serialize)]
struct Message {
    role: String,
    content: String,
}

#[derive(Debug, Deserialize)]
struct ChatResponse {
    choices: Vec<ChatChoice>,
}

#[derive(Debug, Deserialize)]
struct ChatChoice {
    message: ChatMessage,
}

#[derive(Debug, Deserialize)]
struct ChatMessage {
    content: String,
}

pub struct LlmClient {
    http_client: reqwest::blocking::Client,
    config: LlmConfig,
}

impl LlmClient {
    pub fn new(config: LlmConfig) -> Self {
        let http_client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(60))
            .build()
            .expect("Failed to create blocking HTTP client");

        Self {
            http_client,
            config,
        }
    }

    pub fn rewrite(&self, text: &str) -> Result<String> {
        let request = ChatRequest {
            model: self.config.model.clone(),
            messages: vec![
                Message {
                    role: "system".to_string(),
                    content: self.config.system_prompt.clone(),
                },
                Message {
                    role: "user".to_string(),
                    content: text.to_string(),
                },
            ],
            max_tokens: self.config.max_tokens,
            temperature: self.config.temperature,
            stream: false,
        };

        let response = self
            .http_client
            .post(format!("{}/chat/completions", self.config.base_url))
            .header("Authorization", format!("Bearer {}", self.config.api_key))
            .header("Content-Type", "application/json")
            .json(&request)
            .send()
            .map_err(|e| AppError::Network(format!("Failed to send request: {}", e)))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response
                .text()
                .unwrap_or_else(|_| "Unknown error".to_string());
            return Err(AppError::Network(format!(
                "API error ({}): {}",
                status, body
            )));
        }

        let payload: ChatResponse = response
            .json()
            .map_err(|e| AppError::Network(format!("Failed to parse response: {}", e)))?;

        payload
            .choices
            .into_iter()
            .next()
            .map(|choice| choice.message.content.trim().to_string())
            .filter(|content| !content.is_empty())
            .ok_or_else(|| AppError::Network("LLM returned empty content".to_string()))
    }
}

pub fn builtin_providers() -> Vec<LlmProvider> {
    vec![
        LlmProvider {
            id: "openai".to_string(),
            name: "OpenAI".to_string(),
            base_url: "https://api.openai.com/v1".to_string(),
            model: "gpt-4o-mini".to_string(),
        },
        LlmProvider {
            id: "deepseek".to_string(),
            name: "DeepSeek".to_string(),
            base_url: "https://api.deepseek.com/v1".to_string(),
            model: "deepseek-chat".to_string(),
        },
        LlmProvider {
            id: "aliyun".to_string(),
            name: "阿里云百炼".to_string(),
            base_url: "https://dashscope.aliyuncs.com/compatible-mode/v1".to_string(),
            model: "qwen-turbo".to_string(),
        },
        LlmProvider {
            id: "moonshot".to_string(),
            name: "月之暗面".to_string(),
            base_url: "https://api.moonshot.cn/v1".to_string(),
            model: "moonshot-v1-8k".to_string(),
        },
        LlmProvider {
            id: "zhipu".to_string(),
            name: "智谱 AI".to_string(),
            base_url: "https://open.bigmodel.cn/api/paas/v4".to_string(),
            model: "glm-4-flash".to_string(),
        },
        LlmProvider {
            id: "ollama".to_string(),
            name: "Ollama".to_string(),
            base_url: "http://localhost:11434/v1".to_string(),
            model: "qwen2.5:7b".to_string(),
        },
    ]
}

const KEYRING_SERVICE: &str = "shanji-llm";

pub fn load_api_key(provider_id: &str) -> Result<String> {
    keyring::Entry::new(KEYRING_SERVICE, provider_id)
        .and_then(|entry| entry.get_password())
        .map_err(|e| AppError::Internal(e.to_string()))
}

pub fn save_api_key(provider_id: &str, key: &str) -> Result<()> {
    keyring::Entry::new(KEYRING_SERVICE, provider_id)
        .and_then(|entry| entry.set_password(key))
        .map_err(|e| AppError::Internal(e.to_string()))
}

pub fn create_client(provider_id: &str, config: &RewriteConfig) -> Result<LlmClient> {
    let provider = builtin_providers()
        .into_iter()
        .find(|provider| provider.id == provider_id)
        .ok_or_else(|| AppError::InvalidInput(format!("Unknown provider: {}", provider_id)))?;

    let api_key = load_api_key(provider_id)?;
    let system_prompt = config
        .prompts
        .iter()
        .find(|prompt| prompt.id == config.active_prompt_id)
        .map(|prompt| prompt.system_prompt.clone())
        .unwrap_or_else(|| LlmConfig::default().system_prompt);

    Ok(LlmClient::new(LlmConfig {
        base_url: provider.base_url,
        api_key,
        model: provider.model,
        system_prompt,
        max_tokens: 2048,
        temperature: 0.7,
    }))
}
