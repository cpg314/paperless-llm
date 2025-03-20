//! Basic API client for the llama.cpp API server
//! https://github.com/ggml-org/llama.cpp/blob/master/examples/server/README.md
//! NOTE: This is not exactly the OpenAI API (e.g. grammar, timings, props, tokenize)
use anyhow::Context;
use serde::{Deserialize, Serialize};
use tracing::*;

#[derive(Clone)]
pub struct LlamaCpp {
    client: reqwest::Client,
    url: reqwest::Url,
    pub settings: GenerationSettings,
}
#[derive(Serialize, Debug)]
pub struct Query {
    pub stream: bool,
    pub model: String,
    pub messages: Vec<Message>,
    pub grammar: Option<String>,
    pub temperature: f32,
    pub n_predict: usize,
}
#[derive(Serialize, Deserialize, Debug)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
}
#[derive(Serialize, Deserialize, Debug)]
pub struct Message {
    pub role: Role,
    pub content: String,
}
#[derive(Deserialize, Debug)]
pub struct Choice {
    pub message: Message,
}
#[derive(Deserialize, Debug)]
pub struct Response {
    pub choices: Vec<Choice>,
    pub timings: Timings,
}
impl Response {
    pub fn content(&self) -> anyhow::Result<&str> {
        Ok(&self
            .choices
            .first()
            .context("No responses returned")?
            .message
            .content)
    }
}
#[derive(Deserialize, Debug)]
#[allow(dead_code)]
pub struct Timings {
    predicted_ms: f32,
    predicted_n: usize,
    prompt_ms: f32,
    prompt_n: usize,
}
#[derive(Deserialize, Default, Clone, Debug)]
pub struct GenerationSettings {
    pub n_ctx: usize,
}
#[derive(Deserialize, Debug)]
pub struct Props {
    pub default_generation_settings: GenerationSettings,
}
#[derive(Deserialize, Debug)]
pub struct Models {
    pub data: Vec<Model>,
}
#[derive(Deserialize, Debug)]
pub struct Model {
    pub id: String,
}
impl LlamaCpp {
    pub async fn new(url: &reqwest::Url) -> anyhow::Result<Self> {
        let mut s = Self {
            url: url.clone(),
            client: reqwest::Client::new(),
            settings: Default::default(),
        };
        s.settings = s.props().await?.default_generation_settings;
        Ok(s)
    }
    async fn send<T: serde::de::DeserializeOwned>(
        &self,
        r: reqwest::RequestBuilder,
    ) -> anyhow::Result<T> {
        debug!("Sending query");
        Ok(r.send().await?.error_for_status()?.json().await?)
    }
    pub async fn props(&self) -> anyhow::Result<Props> {
        self.send(self.client.get(self.url.join("props")?)).await
    }
    pub async fn models(&self) -> anyhow::Result<Models> {
        self.send(self.client.get(self.url.join("v1/models")?))
            .await
    }
    #[allow(dead_code)]
    pub async fn tokenize(&self, text: &str) -> anyhow::Result<Vec<usize>> {
        #[derive(Serialize)]
        struct Query {
            content: String,
        }
        #[derive(Deserialize)]
        struct Response {
            tokens: Vec<usize>,
        }
        let r: Response = self
            .send(self.client.post(self.url.join("tokenize")?).json(&Query {
                content: text.into(),
            }))
            .await?;
        Ok(r.tokens)
    }
    #[instrument(skip_all)]
    pub async fn completions(&self, query: &Query) -> anyhow::Result<Response> {
        let r: Response = self
            .send(
                self.client
                    .post(self.url.join("v1/chat/completions")?)
                    .json(&query),
            )
            .await?;
        debug!(?r.timings, "Received completion response");
        Ok(r)
    }
}
