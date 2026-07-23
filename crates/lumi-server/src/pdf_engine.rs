//! Sandboxed-process PDF inspection adapter backed by Poppler command-line tools.

use std::fs;
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use lumi_core::{
    content_hash, DiagnosticSeverity, ImportDiagnostic, ImportedPdfResource, InspectedPdf,
    PdfImportError, PdfMetadata, PdfPage, PdfRect, PdfSecurityState, PdfTextBlock, PdfTextLayer,
    PdfTextLayerState, PdfTextLine, PdfTextSpan,
};
use quick_xml::events::{BytesStart, Event};
use quick_xml::Reader;
use uuid::Uuid;

const MAX_TEXT_LAYER_BYTES: u64 = 64 * 1024 * 1024;
const DEFAULT_PAGE_WIDTH: f32 = 612.0;
const DEFAULT_PAGE_HEIGHT: f32 = 792.0;
const DEFAULT_COMMAND_TIMEOUT_SECONDS: u64 = 30;

pub(crate) trait PdfEngine: Send + Sync {
    fn inspect(&self, source: &[u8]) -> Result<InspectedPdf, PdfImportError>;
}

#[derive(Clone, Debug)]
pub(crate) struct PopplerPdfEngine {
    pdfinfo: String,
    pdftotext: String,
    pdftoppm: String,
    command_timeout: Duration,
}

impl PopplerPdfEngine {
    pub(crate) fn from_environment() -> Self {
        Self {
            pdfinfo: std::env::var("LUMI_PDFINFO_BIN").unwrap_or_else(|_| "pdfinfo".to_owned()),
            pdftotext: std::env::var("LUMI_PDFTOTEXT_BIN")
                .unwrap_or_else(|_| "pdftotext".to_owned()),
            pdftoppm: std::env::var("LUMI_PDFTOPPM_BIN").unwrap_or_else(|_| "pdftoppm".to_owned()),
            command_timeout: Duration::from_secs(
                std::env::var("LUMI_PDF_COMMAND_TIMEOUT_SECONDS")
                    .ok()
                    .and_then(|value| value.parse::<u64>().ok())
                    .filter(|seconds| (1..=300).contains(seconds))
                    .unwrap_or(DEFAULT_COMMAND_TIMEOUT_SECONDS),
            ),
        }
    }

    fn command_output(&self, program: &str, args: &[&str]) -> Result<Output, PdfImportError> {
        let mut child = Command::new(program)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| {
                if error.kind() == std::io::ErrorKind::NotFound {
                    PdfImportError::EngineUnavailable(format!("{program} was not found"))
                } else {
                    PdfImportError::EngineUnavailable(error.to_string())
                }
            })?;
        let started = Instant::now();
        let status = loop {
            if let Some(status) = child
                .try_wait()
                .map_err(|error| PdfImportError::EngineUnavailable(error.to_string()))?
            {
                break status;
            }
            if started.elapsed() >= self.command_timeout {
                let _ = child.kill();
                let _ = child.wait();
                return Err(PdfImportError::EngineTimeout);
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        if let Some(mut pipe) = child.stdout.take() {
            pipe.read_to_end(&mut stdout)
                .map_err(|error| PdfImportError::EngineUnavailable(error.to_string()))?;
        }
        if let Some(mut pipe) = child.stderr.take() {
            pipe.read_to_end(&mut stderr)
                .map_err(|error| PdfImportError::EngineUnavailable(error.to_string()))?;
        }
        Ok(Output {
            status,
            stdout,
            stderr,
        })
    }
}

impl PdfEngine for PopplerPdfEngine {
    fn inspect(&self, source: &[u8]) -> Result<InspectedPdf, PdfImportError> {
        let temp = TempPdfDir::create()?;
        let source_path = temp.path.join("source.pdf");
        fs::write(&source_path, source)
            .map_err(|error| PdfImportError::EngineUnavailable(error.to_string()))?;
        let source_arg = path_arg(&source_path)?;
        let info = self.command_output(&self.pdfinfo, &["-box", source_arg])?;
        if !info.status.success() {
            let stderr = String::from_utf8_lossy(&info.stderr);
            if stderr.to_ascii_lowercase().contains("incorrect password")
                || stderr.to_ascii_lowercase().contains("password")
            {
                return Err(PdfImportError::PasswordRequired);
            }
            return Err(PdfImportError::Malformed(redact_engine_error(&stderr)));
        }
        let info_text = String::from_utf8_lossy(&info.stdout);
        let metadata = parse_pdfinfo_metadata(&info_text);
        let page_count = parse_pdfinfo_page_count(&info_text)?;
        let text_path = temp.path.join("text-layer.html");
        let text_arg = path_arg(&text_path)?;
        let text_output = self.command_output(
            &self.pdftotext,
            &["-bbox-layout", "-enc", "UTF-8", source_arg, text_arg],
        )?;
        let mut diagnostics = Vec::new();
        let (mut pages, text_layers) = if text_output.status.success() {
            let text_size = fs::metadata(&text_path)
                .map_err(|error| PdfImportError::Malformed(error.to_string()))?
                .len();
            if text_size > MAX_TEXT_LAYER_BYTES {
                diagnostics.push(warning(
                    "pdf_text_layer_output_exceeded",
                    "Native text extraction exceeded the retained output budget.",
                ));
                (Vec::new(), Vec::new())
            } else {
                parse_bbox_layout(&text_path)?
            }
        } else {
            diagnostics.push(warning(
                "pdf_text_extraction_failed",
                "Native text extraction failed; visual page reading remains available.",
            ));
            (Vec::new(), Vec::new())
        };
        if pages.len() < page_count {
            diagnostics.push(warning(
                "pdf_page_geometry_partial",
                "The PDF engine returned incomplete page geometry; fallback dimensions were used.",
            ));
            for index in pages.len()..page_count {
                pages.push(default_page(index));
            }
        }
        pages.truncate(page_count);
        apply_pdfinfo_fallback_geometry(&info_text, &mut pages);
        let resources = render_first_thumbnail(self, &temp.path, source_arg, &mut diagnostics)?;
        if let Some(resource) = resources.first() {
            if let Some(first_page) = pages.first_mut() {
                first_page.thumbnail_resource_hash = Some(resource.content_hash.clone());
            }
        }
        Ok(InspectedPdf {
            security_state: PdfSecurityState::None,
            metadata,
            pages,
            text_layers,
            outline: Vec::new(),
            links: Vec::new(),
            resources,
            diagnostics,
        })
    }
}

struct TempPdfDir {
    path: PathBuf,
}

impl TempPdfDir {
    fn create() -> Result<Self, PdfImportError> {
        let path = std::env::temp_dir().join(format!("lumi-pdf-{}", Uuid::now_v7()));
        fs::create_dir(&path)
            .map_err(|error| PdfImportError::EngineUnavailable(error.to_string()))?;
        Ok(Self { path })
    }
}

impl Drop for TempPdfDir {
    fn drop(&mut self) {
        if let Err(error) = fs::remove_dir_all(&self.path) {
            tracing::warn!(path = %self.path.display(), %error, "failed to remove PDF worker temp directory");
        }
    }
}

fn path_arg(path: &Path) -> Result<&str, PdfImportError> {
    path.to_str()
        .ok_or_else(|| PdfImportError::EngineUnavailable("temporary path is not UTF-8".to_owned()))
}

fn parse_pdfinfo_metadata(info: &str) -> PdfMetadata {
    PdfMetadata {
        title: info_value(info, "Title"),
        author: info_value(info, "Author"),
        subject: info_value(info, "Subject"),
        keywords: info_value(info, "Keywords")
            .map(|keywords| {
                keywords
                    .split([',', ';'])
                    .map(str::trim)
                    .filter(|keyword| !keyword.is_empty())
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default(),
        creator: info_value(info, "Creator"),
        producer: info_value(info, "Producer"),
        pdf_version: info_value(info, "PDF version"),
    }
}

fn parse_pdfinfo_page_count(info: &str) -> Result<usize, PdfImportError> {
    info_value(info, "Pages")
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|pages| *pages > 0)
        .ok_or_else(|| PdfImportError::Malformed("page count is missing".to_owned()))
}

fn info_value(info: &str, key: &str) -> Option<String> {
    info.lines().find_map(|line| {
        let (candidate, value) = line.split_once(':')?;
        (candidate.trim() == key)
            .then(|| value.trim().to_owned())
            .filter(|value| !value.is_empty())
    })
}

fn parse_bbox_layout(path: &Path) -> Result<(Vec<PdfPage>, Vec<PdfTextLayer>), PdfImportError> {
    let file =
        fs::File::open(path).map_err(|error| PdfImportError::Malformed(error.to_string()))?;
    let mut reader = Reader::from_reader(BufReader::new(file));
    reader.config_mut().trim_text(false);
    let mut buffer = Vec::new();
    let mut pages = Vec::new();
    let mut layers = Vec::new();
    let mut page_builder = None;
    let mut block_builder = None;
    let mut line_builder = None;
    let mut word_builder = None;
    loop {
        match reader
            .read_event_into(&mut buffer)
            .map_err(|error| PdfImportError::Malformed(error.to_string()))?
        {
            Event::Start(element) => match element.local_name().as_ref() {
                b"page" => {
                    let width = attribute_f32(&element, b"width").unwrap_or(DEFAULT_PAGE_WIDTH);
                    let height = attribute_f32(&element, b"height").unwrap_or(DEFAULT_PAGE_HEIGHT);
                    page_builder = Some(PageBuilder::new(pages.len(), width, height));
                }
                b"block" => block_builder = Some(BlockBuilder::from_element(&element)),
                b"line" => line_builder = Some(LineBuilder::from_element(&element)),
                b"word" => word_builder = Some(WordBuilder::from_element(&element)),
                _ => {}
            },
            Event::Text(text) => {
                if let Some(word) = word_builder.as_mut() {
                    let decoded = text
                        .decode()
                        .map_err(|error| PdfImportError::Malformed(error.to_string()))?;
                    word.text.push_str(&decoded);
                }
            }
            Event::End(element) => match element.local_name().as_ref() {
                b"word" => {
                    if let (Some(word), Some(line)) = (word_builder.take(), line_builder.as_mut()) {
                        line.push_word(word);
                    }
                }
                b"line" => {
                    if let (Some(line), Some(block)) = (line_builder.take(), block_builder.as_mut())
                    {
                        block.push_line(line);
                    }
                }
                b"block" => {
                    if let (Some(block), Some(page)) = (block_builder.take(), page_builder.as_mut())
                    {
                        page.blocks.push(block.finish(page.blocks.len()));
                    }
                }
                b"page" => {
                    if let Some(page) = page_builder.take() {
                        let (model, layer) = page.finish();
                        pages.push(model);
                        if let Some(layer) = layer {
                            layers.push(layer);
                        }
                    }
                }
                _ => {}
            },
            Event::Eof => break,
            _ => {}
        }
        buffer.clear();
    }
    Ok((pages, layers))
}

struct PageBuilder {
    index: usize,
    width: f32,
    height: f32,
    blocks: Vec<PdfTextBlock>,
}

impl PageBuilder {
    fn new(index: usize, width: f32, height: f32) -> Self {
        Self {
            index,
            width,
            height,
            blocks: Vec::new(),
        }
    }

    fn finish(self) -> (PdfPage, Option<PdfTextLayer>) {
        let text_state = if self.blocks.is_empty() {
            PdfTextLayerState::None
        } else {
            PdfTextLayerState::Native
        };
        let page_index = u32::try_from(self.index).unwrap_or(u32::MAX);
        let layer = (!self.blocks.is_empty()).then(|| PdfTextLayer {
            page_index,
            extraction_engine: "poppler-pdftotext".to_owned(),
            extraction_revision: "bbox-layout.v1".to_owned(),
            language_hints: Vec::new(),
            reading_order_confidence: 0.6,
            blocks: self.blocks,
        });
        (
            PdfPage {
                page_index,
                page_label: (self.index + 1).to_string(),
                media_box: page_rect(self.width, self.height),
                crop_box: page_rect(self.width, self.height),
                rotation: 0,
                width_points: self.width,
                height_points: self.height,
                page_hash: String::new(),
                text_layer_state: text_state,
                thumbnail_resource_hash: None,
                import_issues: Vec::new(),
            },
            layer,
        )
    }
}

struct BlockBuilder {
    bbox: PdfRect,
    lines: Vec<PdfTextLine>,
}

impl BlockBuilder {
    fn from_element(element: &BytesStart<'_>) -> Self {
        Self {
            bbox: element_rect(element),
            lines: Vec::new(),
        }
    }

    fn push_line(&mut self, line: LineBuilder) {
        self.lines.push(line.finish(self.lines.len()));
    }

    fn finish(self, index: usize) -> PdfTextBlock {
        let text = self
            .lines
            .iter()
            .map(|line| line.text.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        PdfTextBlock {
            block_index: u32::try_from(index).unwrap_or(u32::MAX),
            bbox: self.bbox,
            text,
            lines: self.lines,
        }
    }
}

struct LineBuilder {
    bbox: PdfRect,
    spans: Vec<PdfTextSpan>,
}

impl LineBuilder {
    fn from_element(element: &BytesStart<'_>) -> Self {
        Self {
            bbox: element_rect(element),
            spans: Vec::new(),
        }
    }

    fn push_word(&mut self, word: WordBuilder) {
        if !word.text.is_empty() {
            self.spans.push(word.finish(self.spans.len()));
        }
    }

    fn finish(self, index: usize) -> PdfTextLine {
        let text = self
            .spans
            .iter()
            .map(|span| span.text.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        PdfTextLine {
            line_index: u32::try_from(index).unwrap_or(u32::MAX),
            bbox: self.bbox,
            text,
            spans: self.spans,
        }
    }
}

struct WordBuilder {
    bbox: PdfRect,
    text: String,
}

impl WordBuilder {
    fn from_element(element: &BytesStart<'_>) -> Self {
        Self {
            bbox: element_rect(element),
            text: String::new(),
        }
    }

    fn finish(self, index: usize) -> PdfTextSpan {
        PdfTextSpan {
            span_index: u32::try_from(index).unwrap_or(u32::MAX),
            text: self.text,
            bbox: self.bbox,
            quads: Vec::new(),
            font_name: None,
            font_size: None,
            direction: Some("ltr".to_owned()),
        }
    }
}

fn element_rect(element: &BytesStart<'_>) -> PdfRect {
    let x_min = attribute_f32(element, b"xMin").unwrap_or(0.0);
    let y_min = attribute_f32(element, b"yMin").unwrap_or(0.0);
    let x_max = attribute_f32(element, b"xMax").unwrap_or(x_min);
    let y_max = attribute_f32(element, b"yMax").unwrap_or(y_min);
    PdfRect {
        x: x_min,
        y: y_min,
        width: (x_max - x_min).max(0.0),
        height: (y_max - y_min).max(0.0),
    }
}

fn attribute_f32(element: &BytesStart<'_>, key: &[u8]) -> Option<f32> {
    element.attributes().flatten().find_map(|attribute| {
        (attribute.key.local_name().as_ref() == key)
            .then(|| std::str::from_utf8(attribute.value.as_ref()).ok())
            .flatten()
            .and_then(|value| value.parse::<f32>().ok())
            .filter(|value| value.is_finite())
    })
}

fn page_rect(width: f32, height: f32) -> PdfRect {
    PdfRect {
        x: 0.0,
        y: 0.0,
        width,
        height,
    }
}

fn default_page(index: usize) -> PdfPage {
    PdfPage {
        page_index: u32::try_from(index).unwrap_or(u32::MAX),
        page_label: (index + 1).to_string(),
        media_box: page_rect(DEFAULT_PAGE_WIDTH, DEFAULT_PAGE_HEIGHT),
        crop_box: page_rect(DEFAULT_PAGE_WIDTH, DEFAULT_PAGE_HEIGHT),
        rotation: 0,
        width_points: DEFAULT_PAGE_WIDTH,
        height_points: DEFAULT_PAGE_HEIGHT,
        page_hash: String::new(),
        text_layer_state: PdfTextLayerState::Failed,
        thumbnail_resource_hash: None,
        import_issues: vec!["pdf_page_geometry_fallback".to_owned()],
    }
}

fn apply_pdfinfo_fallback_geometry(info: &str, pages: &mut [PdfPage]) {
    let Some(size) = info_value(info, "Page size") else {
        return;
    };
    let mut tokens = size.split_whitespace();
    let Some(width) = tokens.next().and_then(|value| value.parse::<f32>().ok()) else {
        return;
    };
    let Some(height) = tokens
        .find(|token| *token == "x")
        .and_then(|_| tokens.next())
        .and_then(|value| value.parse::<f32>().ok())
    else {
        return;
    };
    if let Some(page) = pages.first_mut() {
        page.width_points = width;
        page.height_points = height;
        page.media_box = page_rect(width, height);
        page.crop_box = page_rect(width, height);
    }
}

fn render_first_thumbnail(
    engine: &PopplerPdfEngine,
    temp_path: &Path,
    source_arg: &str,
    diagnostics: &mut Vec<ImportDiagnostic>,
) -> Result<Vec<ImportedPdfResource>, PdfImportError> {
    let thumbnail_prefix = temp_path.join("thumbnail");
    let prefix_arg = path_arg(&thumbnail_prefix)?;
    let output = engine.command_output(
        &engine.pdftoppm,
        &[
            "-f",
            "1",
            "-l",
            "1",
            "-singlefile",
            "-scale-to",
            "360",
            "-png",
            source_arg,
            prefix_arg,
        ],
    )?;
    if !output.status.success() {
        diagnostics.push(warning(
            "pdf_thumbnail_failed",
            "The first-page thumbnail could not be generated.",
        ));
        return Ok(Vec::new());
    }
    let bytes = fs::read(thumbnail_prefix.with_extension("png"))
        .map_err(|error| PdfImportError::Malformed(error.to_string()))?;
    Ok(vec![ImportedPdfResource {
        path: "derived/thumbnails/page-0.png".to_owned(),
        media_type: "image/png".to_owned(),
        content_hash: content_hash(&bytes),
        bytes,
    }])
}

fn warning(code: &str, message: &str) -> ImportDiagnostic {
    ImportDiagnostic {
        severity: DiagnosticSeverity::Warning,
        code: code.to_owned(),
        message: message.to_owned(),
        source_path: None,
    }
}

fn redact_engine_error(stderr: &str) -> String {
    stderr
        .lines()
        .next()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .unwrap_or("PDF engine rejected the document")
        .chars()
        .take(240)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn poppler_should_inspect_fixed_layout_fixture() -> Result<(), Box<dyn std::error::Error>> {
        let engine = PopplerPdfEngine::from_environment();
        let source = include_bytes!("../../../tests/fixtures/pdf/text-layer.pdf");
        let inspected = match engine.inspect(source) {
            Ok(inspected) => inspected,
            Err(PdfImportError::EngineUnavailable(error)) => {
                eprintln!("Poppler integration test skipped: {error}");
                return Ok(());
            }
            Err(error) => return Err(error.into()),
        };

        assert_eq!(
            inspected.metadata.title.as_deref(),
            Some("Lumi PDF text layer fixture")
        );
        assert_eq!(inspected.pages.len(), 2);
        assert_eq!(inspected.text_layers.len(), 2);
        assert_eq!(
            inspected.pages[0].text_layer_state,
            PdfTextLayerState::Native
        );
        assert!(inspected.pages[1].width_points > inspected.pages[1].height_points);
        assert!(inspected
            .text_layers
            .iter()
            .flat_map(|layer| &layer.blocks)
            .any(|block| block.text.contains("searchable text")));
        assert_eq!(inspected.resources.len(), 1);
        assert_eq!(inspected.resources[0].media_type, "image/png");
        Ok(())
    }

    #[test]
    fn parse_pdfinfo_metadata_should_extract_safe_fields() {
        let metadata = parse_pdfinfo_metadata(
            "Title:          Test PDF\nAuthor:         Lumi\nPages:          2\nPDF version:    1.7\n",
        );

        assert_eq!(metadata.title.as_deref(), Some("Test PDF"));
        assert_eq!(metadata.author.as_deref(), Some("Lumi"));
    }

    #[test]
    fn redact_engine_error_should_return_only_first_bounded_line() {
        let redacted = redact_engine_error("Syntax Error: broken xref\n/private/source.pdf\n");

        assert_eq!(redacted, "Syntax Error: broken xref");
    }
}
