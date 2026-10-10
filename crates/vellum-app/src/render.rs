//! A background thread that renders pages, so scrolling and zooming never
//! wait on PDFium. The UI asks for (document version, page, resolution) and
//! picks up finished images on a later frame.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};

use vellum_core::pdfium_render::prelude::PdfDocument;
use vellum_core::engine::{self, RenderOptions};

pub struct RenderRequest {
    pub tab_id: u64,
    /// Bumped on every edit; results for older versions are discarded.
    pub version: u64,
    pub bytes: Arc<Vec<u8>>,
    pub password: Option<String>,
    pub page_index: usize,
    pub dpi: f32,
    /// Invert the page colors for comfortable reading in the dark.
    pub night_mode: bool,
}

pub struct RenderResult {
    pub tab_id: u64,
    pub version: u64,
    pub page_index: usize,
    pub dpi: f32,
    pub night_mode: bool,
    pub image: Result<egui::ColorImage, String>,
}

pub struct RenderWorker {
    requests: Sender<RenderRequest>,
    pub results: Receiver<RenderResult>,
}

impl RenderWorker {
    pub fn start(ctx: egui::Context) -> Self {
        let (request_sender, request_receiver) = channel::<RenderRequest>();
        let (result_sender, result_receiver) = channel::<RenderResult>();
        std::thread::Builder::new()
            .name("page renderer".into())
            .spawn(move || run(request_receiver, result_sender, ctx))
            .expect("could not start the page renderer");
        Self { requests: request_sender, results: result_receiver }
    }

    pub fn request(&self, request: RenderRequest) {
        let _ = self.requests.send(request);
    }
}

fn run(requests: Receiver<RenderRequest>, results: Sender<RenderResult>, ctx: egui::Context) {
    let mut documents: HashMap<u64, (u64, PdfDocument<'static>)> = HashMap::new();
    let mut queue: Vec<RenderRequest> = Vec::new();
    loop {
        if queue.is_empty() {
            match requests.recv() {
                Ok(request) => queue.push(request),
                Err(_) => return,
            }
        }
        while let Ok(request) = requests.try_recv() {
            queue.push(request);
        }
        // Newest first: the pages on screen now matter more than ones scrolled past.
        let Some(request) = queue.pop() else { continue };
        let newest_version = queue.iter().chain(std::iter::once(&request)).filter(|r| r.tab_id == request.tab_id).map(|r| r.version).max().unwrap_or(request.version);
        if request.version < newest_version {
            continue;
        }
        let stale = documents.get(&request.tab_id).is_none_or(|(version, _)| *version != request.version);
        if stale {
            documents.remove(&request.tab_id);
            match engine::open_document(request.bytes.as_ref().clone(), request.password.as_deref()) {
                Ok(document) => {
                    documents.insert(request.tab_id, (request.version, document));
                }
                Err(error) => {
                    let _ = results.send(RenderResult { tab_id: request.tab_id, version: request.version, page_index: request.page_index, dpi: request.dpi, night_mode: request.night_mode, image: Err(error.to_string()) });
                    continue;
                }
            }
        }
        let (_, document) = documents.get(&request.tab_id).expect("just inserted");
        let image = engine::render_page(document, request.page_index, &RenderOptions { dpi: request.dpi, ..Default::default() })
            .map(|mut rgba| {
                if request.night_mode {
                    for pixel in rgba.pixels_mut() {
                        // Invert, then squeeze the range so white paper becomes a soft
                        // charcoal (about 28) and black text a gentle off-white (about 230).
                        for channel in &mut pixel.0[..3] {
                            let inverted = 255.0 - *channel as f32;
                            *channel = (28.0 + inverted * (202.0 / 255.0)).round() as u8;
                        }
                    }
                }
                egui::ColorImage::from_rgba_unmultiplied([rgba.width() as usize, rgba.height() as usize], rgba.as_raw())
            })
            .map_err(|error| error.to_string());
        let _ = results.send(RenderResult { tab_id: request.tab_id, version: request.version, page_index: request.page_index, dpi: request.dpi, night_mode: request.night_mode, image });
        ctx.request_repaint();
        // Forget documents for tabs that have been closed or replaced long ago.
        if documents.len() > 8 {
            let keep = request.tab_id;
            documents.retain(|id, _| *id == keep);
        }
    }
}
