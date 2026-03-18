use async_openai::{
    config::OpenAIConfig,
    types::{
        ChatCompletionRequestSystemMessageArgs, ChatCompletionRequestUserMessageArgs,
        CreateChatCompletionRequestArgs,
    },
    Client,
};
use crate::config::RewriteConfig;
use crate::error::{AppError, Result};
use serde::{Deserialize, Serialize};

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

pub struct LlmClient {
    client: Client<OpenAIConfig>,
    config: LlmConfig,
}

impl LlmClient {
    pub fn new(config: LlmConfig) -> Self {
        let openai_config = OpenAIConfig::new()
            .with_api_base(&config.base_url)
            .with_api_key(&config.api_key);
        let client = Client::with_config(openai_config);
        Self { client, config }
    }

    /// 同步改写入口，内部起单线程 tokio runtime 执行异步请求。
    pub fn rewrite(&self, text: &str) -> Result<String> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| AppError::Internal(e.to_string()))?;
        rt.block_on(self.rewrite_async(text))
    }

    async fn rewrite_async(&self, text: &str) -> Result<String> {
        let request = CreateChatCompletionRequestArgs::default()
            .model(&self.config.model)
            .messages(vec![
                ChatCompletionRequestSystemMessageArgs::default()
                    .content(self.config.system_prompt.as_str())
                    .build()
                    .map_err(|e| AppError::Internal(e.to_string()))?
                    .into(),
                ChatCompletionRequestUserMessageArgs::default()
                    .content(text)
                    .build()
                    .map_err(|e| AppError::Internal(e.to_string()))?
                    .into(),
            ])
            .max_tokens(self.config.max_tokens as u32)
            .temperature(self.config.temperature)
            .build()
            .map_err(|e| AppError::Internal(e.to_string()))?;

        let response = self
            .client
            .chat()
            .create(request)
            .await
            .map_err(|e| AppError::Network(friendly_api_error(&e.to_string())))?;

        response
            .choices
            .into_iter()
            .next()
            .and_then(|choice| choice.message.content)
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
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

// XOR 混淆密钥，防止明文存储（不是真正的加密，只是混淆）
const OBFUSCATION_KEY: &[u8] = b"sh4nj1-llm-0bfusc4t10n-k3y-2024";

pub fn encrypt_api_key(key: &str) -> String {
    key.as_bytes()
        .iter()
        .enumerate()
        .map(|(i, b)| b ^ OBFUSCATION_KEY[i % OBFUSCATION_KEY.len()])
        .map(|b| format!("{:02x}", b))
        .collect()
}

pub fn decrypt_api_key(hex: &str) -> String {
    let bytes: Vec<u8> = (0..hex.len())
        .step_by(2)
        .filter_map(|i| u8::from_str_radix(&hex[i..i + 2], 16).ok())
        .collect();
    let decrypted: Vec<u8> = bytes
        .iter()
        .enumerate()
        .map(|(i, b)| b ^ OBFUSCATION_KEY[i % OBFUSCATION_KEY.len()])
        .collect();
    String::from_utf8(decrypted).unwrap_or_default()
}

/// 从内置 provider 列表创建客户端（兼容旧调用）
pub fn create_client(provider_id: &str, config: &RewriteConfig) -> Result<LlmClient> {
    let provider = builtin_providers()
        .into_iter()
        .find(|p| p.id == provider_id)
        .ok_or_else(|| AppError::InvalidInput(format!("Unknown provider: {}", provider_id)))?;

    let api_key = String::new(); // 内置 provider 无存储 key，走 create_client_from_settings
    let system_prompt = system_prompt_from_config(config);

    Ok(LlmClient::new(LlmConfig {
        base_url: provider.base_url,
        api_key,
        model: provider.model,
        system_prompt,
        max_tokens: 2048,
        temperature: 0.7,
    }))
}

/// 从用户在设置页保存的 provider 配置创建客户端（优先使用用户配置）
pub fn create_client_from_settings(config: &RewriteConfig) -> Result<LlmClient> {
    // 优先使用用户自定义配置列表
    let provider = if let Some(p) = config.providers.first() {
        p.clone()
    } else {
        // 回退到内置列表
        let id = &config.active_provider_id;
        builtin_providers()
            .into_iter()
            .find(|p| &p.id == id)
            .map(|p| crate::config::LlmProvider {
                id: p.id,
                name: p.name,
                base_url: p.base_url,
                model: p.model,
                api_key_encrypted: String::new(),
                test_status: 0,
                test_status_text: String::new(),
            })
            .ok_or_else(|| AppError::InvalidInput("No LLM provider configured".to_string()))?
    };

    let api_key = decrypt_api_key(&provider.api_key_encrypted);
    let system_prompt = system_prompt_from_config(config);

    Ok(LlmClient::new(LlmConfig {
        base_url: provider.base_url,
        api_key,
        model: provider.model,
        system_prompt,
        max_tokens: 2048,
        temperature: 0.7,
    }))
}

fn friendly_api_error(raw: &str) -> String {
    let lower = raw.to_lowercase();
    if lower.contains("model not exist") || lower.contains("model_not_found") || lower.contains("does not exist") {
        return "模型不存在，请检查模型名称是否正确".to_string();
    }
    if lower.contains("invalid api key") || lower.contains("authentication") || lower.contains("unauthorized") || lower.contains("401") {
        return "API Key 无效，请检查后重试".to_string();
    }
    if lower.contains("insufficient_quota") || lower.contains("exceeded your current quota") || lower.contains("billing") {
        return "账户余额不足或超出配额".to_string();
    }
    if lower.contains("rate limit") || lower.contains("rate_limit") || lower.contains("429") {
        return "请求频率超限，请稍后重试".to_string();
    }
    if lower.contains("connection") || lower.contains("timeout") || lower.contains("connect error") {
        return "网络连接失败，请检查 Base URL 是否正确".to_string();
    }
    if lower.contains("deserialize") || lower.contains("expected value") {
        return "服务器返回了非预期的响应，请检查 Base URL 是否指向正确的 OpenAI 兼容接口".to_string();
    }
    // 去掉原始错误里的 "Network error: " 前缀，保留 API 返回的核心信息
    raw.trim_start_matches("Network error: ").to_string()
}

fn system_prompt_from_config(config: &RewriteConfig) -> String {
    config
        .prompts
        .iter()
        .find(|p| p.id == config.active_prompt_id)
        .map(|p| p.system_prompt.clone())
        .unwrap_or_else(|| LlmConfig::default().system_prompt)
}
