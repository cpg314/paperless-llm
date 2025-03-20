mod llamacpp;

use anyhow::Context;
use clap::Parser;
use futures::StreamExt;
use tracing::*;
use tracing_indicatif::span_ext::IndicatifSpanExt;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use unicode_truncate::UnicodeTruncateStr;

mod paperless;

/// The tag to identify documents to process. Set is as "inbox tag" in paperless.
const TAG: &str = "llm-process";
/// The name of the custom field containing the amount.
const AMOUNT_FIELD: &str = "Amount";
/// Temperature for sampling
const TEMPERATURE: f32 = 0.0;
/// Maximum response size
const N_PREDICT: usize = 100;

#[derive(Parser, Clone)]
struct Flags {
    #[clap(long)]
    paperless_url: reqwest::Url,
    #[clap(long)]
    paperless_token: String,
    #[clap(long)]
    openai_url: reqwest::Url,
    #[clap(long)]
    apply: bool,
    #[clap(long)]
    process_all: bool,
    #[clap(long, default_value = "CHF")]
    currency: String,
}

#[derive(Clone)]
struct Params {
    model: String,
    paperless: paperless::Paperless,
    llamacpp: llamacpp::LlamaCpp,
    args: Flags,
    field_id: usize,
    tag_id: usize,
}

#[tracing::instrument(skip_all, fields(id=id))]
async fn process_document(id: usize, params: Params) -> anyhow::Result<()> {
    info!("Retrieving document");
    let d = params.paperless.document(id).await?;
    debug!(?d);
    info!(
        length = d.content.len(),
        title = d.title,
        "Retrieved document"
    );

    let n_ctx = params.llamacpp.settings.n_ctx;

    let prompt = include_str!("../prompt.txt").replace("CURRENCY", &params.args.currency);

    // Truncate the document to make sure prompt + doc + output fit in the available context.
    // TODO: This is not great for the amount determination
    // For now, this uses a simple heuristic based on a number of chars per token.
    // TODO: Truncate more if the server refuses the request, or use the tokenizer endpoint first.
    // let tokens = llamacpp.tokenize(&d.content).await?.len();
    // info!(
    //     "Tokens: {} actual / {} estimated / {} factor",
    //     tokens,
    //     d.content.len() as f32 / char_per_token,
    //     d.content.len() as f32 / tokens as f32
    // );
    let char_per_token = 2.5;
    let max_output_tokens = 50;
    let max_doc_size =
        (n_ctx as f32 * char_per_token - prompt.len() as f32 - max_output_tokens as f32).ceil()
            as usize;
    let content = if d.content.len() > max_doc_size {
        warn!(
            original = d.content.len(),
            truncated = max_doc_size,
            "Truncating long document"
        );
        d.content.unicode_truncate(max_doc_size).0
    } else {
        &d.content
    };

    info!("Sending query to LLM");
    let r = params
        .llamacpp
        .completions(&llamacpp::Query {
            messages: vec![
                llamacpp::Message {
                    role: llamacpp::Role::System,
                    content: prompt,
                },
                llamacpp::Message {
                    role: llamacpp::Role::User,
                    content: content.into(),
                },
            ],
            grammar: Some(include_str!("../grammar.gbnf").into()),
            stream: false,
            model: params.model,
            temperature: TEMPERATURE,
            n_predict: N_PREDICT,
        })
        .await?;
    // Parse the structured output
    #[derive(Debug)]
    struct Output {
        title: String,
        amount: Option<f32>,
    }
    impl std::str::FromStr for Output {
        type Err = anyhow::Error;
        fn from_str(s: &str) -> Result<Self, Self::Err> {
            let lines: Vec<&str> = s.lines().collect();
            anyhow::ensure!(lines.len() == 2, "Incorrect number of lines");
            Ok(Self {
                title: lines[0].into(),
                amount: if lines[1] == "-" {
                    None
                } else {
                    Some(lines[1].parse()?)
                },
            })
        }
    }
    let output: Output = r
        .content()?
        .parse()
        .context("Response did not adhere to the structure")?;
    info!(?output, ?r.timings, "Document processed by LLM");
    if d.title != output.title {
        info!("'{}'", prettydiff::diff_words(&d.title, &output.title));
    }
    if params.args.apply {
        let mut d = d;
        info!("Updating document");
        if let Some(amount) = output.amount {
            d.custom_fields = d
                .custom_fields
                .into_iter()
                .filter(|f| f.field != params.field_id)
                .chain(std::iter::once(paperless::CustomFieldValue {
                    field: params.field_id,
                    value: format!("{}{:.2}", params.args.currency, amount).into(),
                }))
                .collect();
        }
        d.tags.retain(|t| *t != params.tag_id);
        let patch = serde_json::json!({"title": output.title, "tags": d.tags, "custom_fields": d.custom_fields });
        debug!(?patch, "Computed patch");
        params.paperless.patch_document(
            id,
            serde_json::json!({"title": output.title, "tags": d.tags, "custom_fields": d.custom_fields }),
        ).await?;
    }
    Ok(())
}

#[tokio::main]
async fn main() {
    let indicatif_layer =
        tracing_indicatif::IndicatifLayer::new().with_max_progress_bars(100, None);
    let filter_layer = tracing_subscriber::EnvFilter::try_from_default_env()
        .or_else(|_| tracing_subscriber::EnvFilter::try_new("info"))
        .unwrap();
    tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer().with_writer(indicatif_layer.get_stderr_writer()))
        .with(filter_layer)
        .with(indicatif_layer)
        .init();
    let args = Flags::parse();
    if let Err(e) = main_impl(args).await {
        error!("{:?}", e);
    }
}

fn warn_apply(args: &Flags) {
    if !args.apply {
        warn!("Not applying changes, use the --apply flag");
    }
}
async fn main_impl(args: Flags) -> anyhow::Result<()> {
    let start = std::time::Instant::now();

    if args.apply {
        let confirmation = dialoguer::Confirm::new()
            .with_prompt("Are you sure you want to automatically apply changes? No backup of the previous titles will be done outside of the logs.")
            .default(false)
            .interact()
            ?;
        anyhow::ensure!(confirmation, "User aborted");
    }
    warn_apply(&args);

    info!("Retrieving documents from paperless");
    let paperless = paperless::Paperless::new(args.paperless_url.clone(), &args.paperless_token);
    let field_id = *paperless
        .custom_fields()
        .await?
        .get(AMOUNT_FIELD)
        .context("Failed to find amount custom field")?;
    let tag_id = *paperless
        .tags()
        .await?
        .get(TAG)
        .context("Failed to find tag")?;
    let mut d: Vec<usize> = if args.process_all {
        paperless.documents(&[]).await?
    } else {
        paperless.documents_with_tag(TAG).await?
    };
    d.sort();
    info!("Found {} documents (with tag {}) to process", d.len(), TAG);

    info!("Selecting model");
    let llamacpp = llamacpp::LlamaCpp::new(&args.openai_url).await?;
    let models = llamacpp
        .models()
        .await
        .context("Failed to retrieve models")?;
    let model = &models.data.first().context("No model found")?.id;
    info!(model, ctx = llamacpp.settings.n_ctx, "Selected model");

    let span = info_span!("process");
    span.pb_set_style(&indicatif::ProgressStyle::with_template(
        "{wide_bar} {pos}/{len} ({percent}%) ETA {eta}",
    )?);
    span.pb_set_length(d.len() as u64);
    let _span = span.enter();
    info!("Processing all {} documents", d.len());

    let params = Params {
        model: model.into(),
        paperless,
        llamacpp,
        args: args.clone(),
        field_id,
        tag_id,
    };
    let failed = futures::stream::iter(d)
        .map(|d| {
            let params = params.clone();
            async move {
                let r = process_document(d, params)
                    .await
                    .inspect_err(|e| error!("Error processing document {}: {:?}", d, e))
                    .err()
                    .map(|_| d);
                Span::current().pb_inc(1);
                r
            }
        })
        .buffer_unordered(10)
        .filter_map(futures::future::ready)
        .collect::<Vec<usize>>()
        .await;
    drop(_span);
    info!(elapsed=?start.elapsed(), "Done processing everything");
    if !failed.is_empty() {
        error!(?failed, "{} documents failed processing", failed.len());
    }
    warn_apply(&args);

    Ok(())
}
