//! Basic API client for the `paperless-ngx` REST API
//! See https://docs.paperless-ngx.com/api/
//! TODO: See if any of the crates in https://crates.io/search?q=paperless are good enough.
use std::collections::HashMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tracing::*;

#[derive(Clone)]
pub struct Paperless {
    client: reqwest::Client,
    token: String,
    api_url: reqwest::Url,
    limiter: Arc<governor::DefaultDirectRateLimiter>,
}
#[derive(Debug, Serialize, Deserialize)]
pub struct CustomFieldValue {
    pub field: usize,
    pub value: serde_json::Value,
}
#[derive(Debug, Deserialize)]
pub struct DocumentResponse {
    pub content: String,
    pub title: String,
    pub custom_fields: Vec<CustomFieldValue>,
    pub tags: Vec<usize>,
}
#[derive(Debug, Deserialize)]
struct Results<T> {
    results: Vec<T>,
}
impl Paperless {
    pub fn new(url: reqwest::Url, token: &str) -> Self {
        Self {
            api_url: url.join("/api/").unwrap(),
            token: token.into(),
            client: reqwest::Client::new(),
            limiter: Arc::new(governor::RateLimiter::direct(governor::Quota::per_second(
                std::num::NonZero::new(10_u32).unwrap(),
            ))),
        }
    }
    pub async fn query<T: serde::de::DeserializeOwned>(
        &self,
        r: reqwest::RequestBuilder,
    ) -> anyhow::Result<T> {
        debug!("Executing query");
        self.limiter.until_ready().await;
        Ok(r.header(
            reqwest::header::AUTHORIZATION,
            format!("Token {}", self.token),
        )
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?)
    }
    pub async fn custom_fields(&self) -> anyhow::Result<HashMap<String, usize>> {
        self.id_name("custom_fields/").await
    }
    pub async fn tags(&self) -> anyhow::Result<HashMap<String, usize>> {
        self.id_name("tags/").await
    }
    async fn id_name(&self, method: &str) -> anyhow::Result<HashMap<String, usize>> {
        #[derive(Debug, Deserialize)]
        pub struct IdName {
            id: usize,
            name: String,
        }
        self.query::<Results<IdName>>(self.client.get(self.api_url.join(method)?))
            .await
            .map(|r| r.results.into_iter().map(|x| (x.name, x.id)).collect())
    }
    pub async fn documents_with_tag(&self, tag: &str) -> anyhow::Result<Vec<usize>> {
        self.documents(&[("tags__name__iexact", tag)]).await
    }
    #[instrument(skip(self, payload))]
    pub async fn patch_document(
        &self,
        id: usize,
        payload: serde_json::Value,
    ) -> anyhow::Result<()> {
        self.query::<serde_json::Value>(self.client.patch(self.document_url(id)?).json(&payload))
            .await
            .map(|_| ())
    }
    #[instrument(skip(self))]
    pub async fn documents(&self, query: &[(&str, &str)]) -> anyhow::Result<Vec<usize>> {
        #[derive(Debug, Deserialize)]
        struct DocumentsResponse {
            all: Vec<usize>,
        }
        self.query(
            self.client
                .get(self.api_url.join("documents/")?)
                .query(query),
        )
        .await
        .map(|d: DocumentsResponse| d.all)
    }
    #[instrument(skip(self))]
    fn document_url(&self, id: usize) -> anyhow::Result<reqwest::Url> {
        Ok(self.api_url.join(&format!("documents/{}/", id))?)
    }
    #[instrument(skip(self))]
    pub async fn document(&self, id: usize) -> anyhow::Result<DocumentResponse> {
        self.query(self.client.get(self.document_url(id)?)).await
    }
}
