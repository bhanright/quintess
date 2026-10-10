//! AI assistance through OpenRouter: summaries, questions answered with
//! page references, translation, alternate text for images, and suggested
//! bookmarks. Nothing is sent anywhere unless one of these is called.

use anyhow::{Context, Result, anyhow, bail};
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::settings::Settings;

const ENDPOINT: &str = "https://openrouter.ai/api/v1/chat/completions";
/// Document text beyond this many characters is cut, to bound cost.
const MAX_DOCUMENT_CHARACTERS: usize = 600_000;

/// One part of a message: text or an image.
pub enum Part {
    Text(String),
    PngImage(Vec<u8>),
}

/// Sends one request and returns the reply text.
pub fn complete(settings: &Settings, instructions: &str, parts: Vec<Part>) -> Result<String> {
    let key = settings.api_key().ok_or_else(|| anyhow!("no OpenRouter key: set OPENROUTER_API_KEY or add it in Settings"))?;
    let content: Vec<serde_json::Value> = parts
        .into_iter()
        .map(|part| match part {
            Part::Text(text) => json!({ "type": "text", "text": text }),
            Part::PngImage(bytes) => json!({ "type": "image_url", "image_url": { "url": format!("data:image/png;base64,{}", base64::engine::general_purpose::STANDARD.encode(bytes)) } }),
        })
        .collect();
    let body = json!({
        "model": settings.ai_model,
        "messages": [
            { "role": "system", "content": instructions },
            { "role": "user", "content": content },
        ],
    });
    let mut response = ureq::post(ENDPOINT)
        .header("Authorization", &format!("Bearer {key}"))
        .header("X-Title", "Vellum")
        .header("Content-Type", "application/json")
        .config()
        .http_status_as_error(false)
        .build()
        .send(serde_json::to_string(&body)?)
        .context("could not reach OpenRouter")?;
    let status = response.status();
    let text = response.body_mut().with_config().limit(32 * 1024 * 1024).read_to_string()?;
    if !status.is_success() {
        let message = serde_json::from_str::<serde_json::Value>(&text).ok().and_then(|v| v["error"]["message"].as_str().map(str::to_string)).unwrap_or(text);
        bail!("OpenRouter returned {status}: {message}");
    }
    let value: serde_json::Value = serde_json::from_str(&text)?;
    value["choices"][0]["message"]["content"].as_str().map(|s| s.trim().to_string()).ok_or_else(|| anyhow!("OpenRouter sent no answer"))
}

/// The document's text with page markers, trimmed to the cost limit.
pub fn document_text(document: &pdfium_render::prelude::PdfDocument) -> Result<String> {
    let mut text = String::new();
    for (index, page) in document.pages().iter().enumerate() {
        text.push_str(&format!("\n[Page {}]\n", index + 1));
        text.push_str(&page.text()?.all());
        if text.len() > MAX_DOCUMENT_CHARACTERS {
            text.truncate(MAX_DOCUMENT_CHARACTERS);
            text.push_str("\n[The rest of the document was cut for length.]");
            break;
        }
    }
    Ok(text)
}

pub fn summarize(settings: &Settings, document_text: &str, length: &str) -> Result<String> {
    complete(
        settings,
        "You summarize documents accurately for a busy reader. Use American English. Write plain prose or short bullet lists; do not invent facts. Cite pages like (p. 3) for key points.",
        vec![Part::Text(format!("Summarize this document ({length}).\n\n{document_text}"))],
    )
}

pub fn ask(settings: &Settings, document_text: &str, question: &str) -> Result<String> {
    complete(
        settings,
        "You answer questions about the given document only, using American English. Quote or cite the pages you rely on, like (p. 4). If the document does not answer the question, say so plainly.",
        vec![Part::Text(format!("Document:\n{document_text}\n\nQuestion: {question}"))],
    )
}

pub fn translate(settings: &Settings, text: &str, language: &str) -> Result<String> {
    complete(
        settings,
        "You are a professional translator. Translate faithfully, keeping paragraph breaks, numbers and names. Return only the translation.",
        vec![Part::Text(format!("Translate into {language}:\n\n{text}"))],
    )
}

/// Alternate text for an image, for screen readers.
pub fn describe_image(settings: &Settings, png: Vec<u8>, context: &str) -> Result<String> {
    complete(
        settings,
        "Write alternate text for an image in a document, for people using screen readers. One or two sentences in American English describing what matters in the image. Do not start with 'Image of'. For charts, state the main finding.",
        vec![Part::PngImage(png), Part::Text(format!("Surrounding text, for context: {context}"))],
    )
}

#[derive(Debug, Deserialize, Serialize)]
struct SuggestedBookmark {
    title: String,
    page: u32,
    #[serde(default)]
    children: Vec<SuggestedBookmark>,
}

/// An outline built from the document's content.
pub fn suggest_bookmarks(settings: &Settings, document_text: &str) -> Result<Vec<crate::bookmarks::BookmarkEntry>> {
    let reply = complete(
        settings,
        "You build a table of contents for a document. Reply with JSON only: an array of objects {\"title\": string, \"page\": number, \"children\": [...]} using the [Page N] markers for page numbers. Two levels at most. Titles in the document's own words.",
        vec![Part::Text(document_text.to_string())],
    )?;
    let json_start = reply.find('[').ok_or_else(|| anyhow!("the reply had no outline"))?;
    let json_end = reply.rfind(']').ok_or_else(|| anyhow!("the reply had no outline"))? + 1;
    let suggested: Vec<SuggestedBookmark> = serde_json::from_str(&reply[json_start..json_end]).context("the outline could not be read")?;
    fn convert(items: Vec<SuggestedBookmark>) -> Vec<crate::bookmarks::BookmarkEntry> {
        items.into_iter().map(|item| crate::bookmarks::BookmarkEntry { title: item.title, page: item.page.max(1), children: convert(item.children) }).collect()
    }
    Ok(convert(suggested))
}
