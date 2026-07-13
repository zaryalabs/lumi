//! EPUB container/parser/sanitizer probe from ADR 0005.

use std::collections::{HashMap, HashSet};
use std::io::{Cursor, Read, Write};

use ammonia::Builder;
use quick_xml::events::{BytesStart, Event};
use quick_xml::{Reader, XmlVersion};
use scraper::{Html, Selector};
use thiserror::Error;
use url::Url;
use zip::read::ZipArchive;
use zip::write::{SimpleFileOptions, ZipWriter};
use zip::CompressionMethod;

const MIB: u64 = 1024 * 1024;
const EPUB_MIMETYPE: &[u8] = b"application/epub+zip";

/// Versioned defensive limits selected for the first real EPUB importer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EpubLimits {
    /// Maximum source archive size.
    pub source_bytes: u64,
    /// Maximum number of ZIP entries.
    pub entries: usize,
    /// Maximum sum of declared uncompressed sizes.
    pub expanded_bytes: u64,
    /// Maximum size of one generic resource.
    pub resource_bytes: u64,
    /// Maximum size of container, OPF or NCX XML.
    pub package_xml_bytes: u64,
    /// Maximum size of one XHTML/HTML content document.
    pub content_document_bytes: u64,
    /// Maximum normalized archive path length in bytes.
    pub path_bytes: usize,
    /// Maximum compressed-to-expanded ratio for one entry and the archive.
    pub compression_ratio: u64,
}

impl EpubLimits {
    /// Return the accepted `epub-limits.s1` profile.
    #[must_use]
    pub const fn s1() -> Self {
        Self {
            source_bytes: 100 * MIB,
            entries: 10_000,
            expanded_bytes: 512 * MIB,
            resource_bytes: 64 * MIB,
            package_xml_bytes: 2 * MIB,
            content_document_bytes: 8 * MIB,
            path_bytes: 1024,
            compression_ratio: 100,
        }
    }
}

/// Successful output proving that all selected parsing layers were exercised.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EpubProbeReport {
    /// Number of archive entries validated before extraction.
    pub entry_count: usize,
    /// EPUB package title read through `quick-xml`.
    pub title: String,
    /// Number of linear spine items resolved through the OPF manifest.
    pub spine_item_count: usize,
    /// Number of navigation links parsed through `scraper`.
    pub navigation_item_count: usize,
    /// Number of normalized paragraph candidates in the first spine item.
    pub paragraph_count: usize,
    /// Whether script, handler and `javascript:` payloads were removed.
    pub active_content_removed: bool,
}

/// Stable failure classes needed by import diagnostics and security tests.
#[derive(Debug, Error)]
pub enum EpubProbeError {
    /// Source archive is larger than the accepted upload profile.
    #[error("source EPUB is {actual} bytes; limit is {limit}")]
    SourceTooLarge {
        /// Observed source size.
        actual: u64,
        /// Configured source limit.
        limit: u64,
    },
    /// Archive has too many entries.
    #[error("EPUB has {actual} ZIP entries; limit is {limit}")]
    TooManyEntries {
        /// Observed entry count.
        actual: usize,
        /// Configured entry-count limit.
        limit: usize,
    },
    /// Entry path is unsafe or too long.
    #[error("unsafe EPUB path: {0}")]
    UnsafePath(String),
    /// Two ZIP entries normalize to the same archive path.
    #[error("duplicate EPUB path: {0}")]
    DuplicatePath(String),
    /// Entry uses a ZIP feature outside EPUB OCF.
    #[error("unsupported ZIP entry `{path}`: {reason}")]
    UnsupportedZipEntry {
        /// Safe diagnostic path.
        path: String,
        /// Stable reason for rejecting the ZIP feature.
        reason: &'static str,
    },
    /// Declared or observed resource size exceeds a defensive limit.
    #[error("EPUB entry `{path}` is {actual} bytes; limit is {limit}")]
    EntryTooLarge {
        /// Safe diagnostic path.
        path: String,
        /// Observed expanded size.
        actual: u64,
        /// Configured per-entry limit.
        limit: u64,
    },
    /// Declared total uncompressed size exceeds the profile.
    #[error("EPUB expands to {actual} bytes; limit is {limit}")]
    ExpandedSizeExceeded {
        /// Observed sum of expanded entry sizes.
        actual: u64,
        /// Configured aggregate expanded-size limit.
        limit: u64,
    },
    /// An entry or the full archive exceeds the compression-ratio limit.
    #[error("compression ratio for `{path}` exceeds {limit}:1")]
    CompressionRatioExceeded {
        /// Entry path or aggregate archive marker.
        path: String,
        /// Configured maximum ratio.
        limit: u64,
    },
    /// Required OCF entry is absent or invalid.
    #[error("invalid EPUB container: {0}")]
    InvalidContainer(&'static str),
    /// Required package or content entry was not found.
    #[error("EPUB entry `{0}` was not found")]
    MissingEntry(String),
    /// XML could not be parsed into the required package shape.
    #[error("EPUB XML error: {0}")]
    Xml(String),
    /// Package-relative URL escaped the archive or used a remote origin.
    #[error("invalid EPUB reference `{0}`")]
    InvalidReference(String),
    /// HTML selector used by the probe could not be compiled.
    #[error("internal HTML selector is invalid: {0}")]
    HtmlSelector(String),
    /// ZIP reader/writer failure.
    #[error(transparent)]
    Zip(#[from] zip::result::ZipError),
    /// Bounded streaming read/write failure.
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// Build and inspect the deterministic Stage 0 EPUB 3 fixture.
///
/// # Errors
///
/// Returns [`EpubProbeError`] if fixture construction or any selected parser,
/// sanitizer or container check fails.
pub fn run_epub_probe() -> Result<EpubProbeReport, EpubProbeError> {
    let fixture = build_fixture_epub()?;
    inspect_epub(&fixture, EpubLimits::s1())
}

/// Inspect one EPUB using the accepted Stage 0 stack and defensive limits.
///
/// # Errors
///
/// Returns [`EpubProbeError`] for OCF violations, limit violations, unresolved
/// package references or parser failures.
pub fn inspect_epub(source: &[u8], limits: EpubLimits) -> Result<EpubProbeReport, EpubProbeError> {
    let source_size = u64::try_from(source.len()).unwrap_or(u64::MAX);
    if source_size > limits.source_bytes {
        return Err(EpubProbeError::SourceTooLarge {
            actual: source_size,
            limit: limits.source_bytes,
        });
    }

    let mut archive = ZipArchive::new(Cursor::new(source))?;
    let summary = validate_archive(&mut archive, limits)?;
    let mimetype = read_entry(&mut archive, "mimetype", limits.package_xml_bytes)?;
    if mimetype != EPUB_MIMETYPE {
        return Err(EpubProbeError::InvalidContainer(
            "mimetype must contain application/epub+zip without padding",
        ));
    }

    let container = read_entry(
        &mut archive,
        "META-INF/container.xml",
        limits.package_xml_bytes,
    )?;
    let package_path = parse_container_path(&container)?;
    ensure_known_entry(&summary.names, &package_path)?;
    let package = read_entry(&mut archive, &package_path, limits.package_xml_bytes)?;
    let package_model = parse_package(&package)?;

    let content_href = package_model
        .spine
        .first()
        .and_then(|idref| package_model.manifest.get(idref))
        .map(|item| item.href.as_str())
        .ok_or(EpubProbeError::InvalidContainer(
            "OPF spine does not resolve to a manifest item",
        ))?;
    let content_path = resolve_package_reference(&package_path, content_href)?;
    ensure_known_entry(&summary.names, &content_path)?;
    let content = read_entry(&mut archive, &content_path, limits.content_document_bytes)?;
    let normalized = inspect_content_document(&content)?;

    let navigation_item_count = if let Some(nav_href) = package_model.nav_href.as_deref() {
        let nav_path = resolve_package_reference(&package_path, nav_href)?;
        ensure_known_entry(&summary.names, &nav_path)?;
        let nav = read_entry(&mut archive, &nav_path, limits.content_document_bytes)?;
        count_navigation_items(&nav)?
    } else {
        0
    };

    Ok(EpubProbeReport {
        entry_count: summary.entry_count,
        title: package_model.title,
        spine_item_count: package_model.spine.len(),
        navigation_item_count,
        paragraph_count: normalized.paragraph_count,
        active_content_removed: normalized.active_content_removed,
    })
}

#[derive(Debug)]
struct ArchiveSummary {
    entry_count: usize,
    names: HashSet<String>,
}

fn validate_archive(
    archive: &mut ZipArchive<Cursor<&[u8]>>,
    limits: EpubLimits,
) -> Result<ArchiveSummary, EpubProbeError> {
    if archive.len() > limits.entries {
        return Err(EpubProbeError::TooManyEntries {
            actual: archive.len(),
            limit: limits.entries,
        });
    }

    let mut names = HashSet::with_capacity(archive.len());
    let mut expanded_bytes = 0_u64;
    let mut compressed_bytes = 0_u64;

    for index in 0..archive.len() {
        let entry = archive.by_index(index)?;
        let raw_name = entry.name().to_owned();
        let enclosed = entry
            .enclosed_name()
            .ok_or_else(|| EpubProbeError::UnsafePath(raw_name.clone()))?;
        let path = enclosed
            .to_str()
            .ok_or_else(|| EpubProbeError::UnsafePath(raw_name.clone()))?
            .replace('\\', "/");

        if path.len() > limits.path_bytes {
            return Err(EpubProbeError::UnsafePath(path));
        }
        if entry.is_symlink() {
            return Err(EpubProbeError::UnsupportedZipEntry {
                path,
                reason: "symbolic links are not EPUB resources",
            });
        }
        if entry.encrypted() {
            return Err(EpubProbeError::UnsupportedZipEntry {
                path,
                reason: "ZIP encryption is forbidden by EPUB OCF",
            });
        }
        if !matches!(
            entry.compression(),
            CompressionMethod::Stored | CompressionMethod::Deflated
        ) {
            return Err(EpubProbeError::UnsupportedZipEntry {
                path,
                reason: "only stored and Deflate entries are supported",
            });
        }
        if index == 0 && (path != "mimetype" || entry.compression() != CompressionMethod::Stored) {
            return Err(EpubProbeError::InvalidContainer(
                "mimetype must be the first stored ZIP entry",
            ));
        }
        if entry.size() > limits.resource_bytes {
            return Err(EpubProbeError::EntryTooLarge {
                path,
                actual: entry.size(),
                limit: limits.resource_bytes,
            });
        }
        ensure_ratio(
            entry.size(),
            entry.compressed_size(),
            limits.compression_ratio,
            &path,
        )?;

        expanded_bytes = expanded_bytes.saturating_add(entry.size());
        compressed_bytes = compressed_bytes.saturating_add(entry.compressed_size());
        if !names.insert(path.clone()) {
            return Err(EpubProbeError::DuplicatePath(path));
        }
    }

    if expanded_bytes > limits.expanded_bytes {
        return Err(EpubProbeError::ExpandedSizeExceeded {
            actual: expanded_bytes,
            limit: limits.expanded_bytes,
        });
    }
    ensure_ratio(
        expanded_bytes,
        compressed_bytes,
        limits.compression_ratio,
        "<archive>",
    )?;

    Ok(ArchiveSummary {
        entry_count: archive.len(),
        names,
    })
}

fn ensure_ratio(
    expanded: u64,
    compressed: u64,
    limit: u64,
    path: &str,
) -> Result<(), EpubProbeError> {
    if expanded == 0 {
        return Ok(());
    }
    if compressed == 0 || expanded > compressed.saturating_mul(limit) {
        return Err(EpubProbeError::CompressionRatioExceeded {
            path: path.to_owned(),
            limit,
        });
    }
    Ok(())
}

fn ensure_known_entry(names: &HashSet<String>, path: &str) -> Result<(), EpubProbeError> {
    if names.contains(path) {
        Ok(())
    } else {
        Err(EpubProbeError::MissingEntry(path.to_owned()))
    }
}

fn read_entry(
    archive: &mut ZipArchive<Cursor<&[u8]>>,
    path: &str,
    limit: u64,
) -> Result<Vec<u8>, EpubProbeError> {
    let entry = archive
        .by_name(path)
        .map_err(|_| EpubProbeError::MissingEntry(path.to_owned()))?;
    if entry.size() > limit {
        return Err(EpubProbeError::EntryTooLarge {
            path: path.to_owned(),
            actual: entry.size(),
            limit,
        });
    }

    let mut bytes = Vec::new();
    entry
        .take(limit.saturating_add(1))
        .read_to_end(&mut bytes)?;
    let actual = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
    if actual > limit {
        return Err(EpubProbeError::EntryTooLarge {
            path: path.to_owned(),
            actual,
            limit,
        });
    }
    Ok(bytes)
}

fn parse_container_path(xml: &[u8]) -> Result<String, EpubProbeError> {
    let mut reader = Reader::from_reader(xml);
    reader.config_mut().trim_text(true);

    loop {
        match reader.read_event() {
            Ok(Event::Empty(element) | Event::Start(element))
                if element.local_name().as_ref() == b"rootfile" =>
            {
                let attributes = attributes(&element)?;
                return attributes.get("full-path").cloned().ok_or(
                    EpubProbeError::InvalidContainer("container rootfile has no full-path"),
                );
            }
            Ok(Event::Eof) => {
                return Err(EpubProbeError::InvalidContainer(
                    "container.xml has no rootfile",
                ));
            }
            Ok(Event::DocType(_)) => {
                return Err(EpubProbeError::Xml(
                    "DOCTYPE is forbidden in EPUB package XML".to_owned(),
                ));
            }
            Ok(_) => {}
            Err(error) => return Err(EpubProbeError::Xml(error.to_string())),
        }
    }
}

#[derive(Debug)]
struct PackageItem {
    href: String,
}

#[derive(Debug)]
struct PackageModel {
    title: String,
    manifest: HashMap<String, PackageItem>,
    spine: Vec<String>,
    nav_href: Option<String>,
}

fn parse_package(xml: &[u8]) -> Result<PackageModel, EpubProbeError> {
    let mut reader = Reader::from_reader(xml);
    reader.config_mut().trim_text(true);
    let mut in_title = false;
    let mut title = String::new();
    let mut manifest = HashMap::new();
    let mut spine = Vec::new();
    let mut nav_href = None;

    loop {
        match reader.read_event() {
            Ok(Event::Start(element)) if element.local_name().as_ref() == b"title" => {
                in_title = true;
            }
            Ok(Event::End(element)) if element.local_name().as_ref() == b"title" => {
                in_title = false;
            }
            Ok(Event::Text(text)) if in_title => {
                title.push_str(
                    &text
                        .decode()
                        .map_err(|error| EpubProbeError::Xml(error.to_string()))?,
                );
            }
            Ok(Event::Empty(element) | Event::Start(element))
                if element.local_name().as_ref() == b"item" =>
            {
                let attributes = attributes(&element)?;
                let id = required_attribute(&attributes, "id")?;
                let href = required_attribute(&attributes, "href")?;
                let properties = attributes.get("properties").cloned().unwrap_or_default();
                if nav_href.is_none() && properties.split_whitespace().any(|value| value == "nav") {
                    nav_href = Some(href.clone());
                }
                manifest.insert(id, PackageItem { href });
            }
            Ok(Event::Empty(element) | Event::Start(element))
                if element.local_name().as_ref() == b"itemref" =>
            {
                let attributes = attributes(&element)?;
                spine.push(required_attribute(&attributes, "idref")?);
            }
            Ok(Event::DocType(_)) => {
                return Err(EpubProbeError::Xml(
                    "DOCTYPE is forbidden in EPUB package XML".to_owned(),
                ));
            }
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(error) => return Err(EpubProbeError::Xml(error.to_string())),
        }
    }

    if title.is_empty() {
        return Err(EpubProbeError::InvalidContainer("OPF title is empty"));
    }
    Ok(PackageModel {
        title,
        manifest,
        spine,
        nav_href,
    })
}

fn attributes(element: &BytesStart<'_>) -> Result<HashMap<String, String>, EpubProbeError> {
    let mut values = HashMap::new();
    for attribute in element.attributes() {
        let attribute = attribute.map_err(|error| EpubProbeError::Xml(error.to_string()))?;
        let key = String::from_utf8_lossy(attribute.key.local_name().as_ref()).into_owned();
        let value = attribute
            .normalized_value(XmlVersion::Implicit1_0)
            .map_err(|error| EpubProbeError::Xml(error.to_string()))?
            .into_owned();
        values.insert(key, value);
    }
    Ok(values)
}

fn required_attribute(
    attributes: &HashMap<String, String>,
    name: &'static str,
) -> Result<String, EpubProbeError> {
    attributes
        .get(name)
        .cloned()
        .ok_or(EpubProbeError::InvalidContainer(
            "OPF element misses a required attribute",
        ))
}

fn resolve_package_reference(package_path: &str, href: &str) -> Result<String, EpubProbeError> {
    let base = Url::parse(&format!("https://lumi.invalid/{package_path}"))
        .map_err(|_| EpubProbeError::InvalidReference(href.to_owned()))?;
    let resolved = base
        .join(href)
        .map_err(|_| EpubProbeError::InvalidReference(href.to_owned()))?;
    if resolved.origin() != base.origin() || resolved.cannot_be_a_base() {
        return Err(EpubProbeError::InvalidReference(href.to_owned()));
    }

    let path = resolved.path().trim_start_matches('/').to_owned();
    if path.is_empty() || path.split('/').any(|segment| segment == "..") {
        return Err(EpubProbeError::InvalidReference(href.to_owned()));
    }
    Ok(path)
}

#[derive(Debug)]
struct ContentInspection {
    paragraph_count: usize,
    active_content_removed: bool,
}

fn inspect_content_document(content: &[u8]) -> Result<ContentInspection, EpubProbeError> {
    let source = String::from_utf8_lossy(content);
    let allowed_tags: HashSet<&str> = [
        "html", "head", "title", "body", "section", "h1", "h2", "p", "a", "em", "strong", "aside",
        "img",
    ]
    .into_iter()
    .collect();
    let mut sanitizer = Builder::default();
    sanitizer.tags(allowed_tags);
    let cleaned = sanitizer.clean(&source).to_string();
    let lowercase = cleaned.to_ascii_lowercase();
    let active_content_removed = !lowercase.contains("<script")
        && !lowercase.contains("onclick")
        && !lowercase.contains("onerror")
        && !lowercase.contains("javascript:");

    let document = Html::parse_document(&cleaned);
    let paragraphs = selector("p")?;
    let paragraph_count = document.select(&paragraphs).count();

    Ok(ContentInspection {
        paragraph_count,
        active_content_removed,
    })
}

fn count_navigation_items(content: &[u8]) -> Result<usize, EpubProbeError> {
    let document = Html::parse_document(&String::from_utf8_lossy(content));
    let links = selector("nav a")?;
    Ok(document.select(&links).count())
}

fn selector(value: &str) -> Result<Selector, EpubProbeError> {
    Selector::parse(value).map_err(|error| EpubProbeError::HtmlSelector(error.to_string()))
}

fn build_fixture_epub() -> Result<Vec<u8>, EpubProbeError> {
    let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
    let stored = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
    let deflated = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);

    write_entry(&mut writer, "mimetype", EPUB_MIMETYPE, stored)?;
    write_entry(
        &mut writer,
        "META-INF/container.xml",
        br#"<?xml version="1.0"?>
<container xmlns="urn:oasis:names:tc:opendocument:xmlns:container" version="1.0">
  <rootfiles><rootfile full-path="EPUB/package.opf" media-type="application/oebps-package+xml"/></rootfiles>
</container>"#,
        deflated,
    )?;
    write_entry(
        &mut writer,
        "EPUB/package.opf",
        br#"<?xml version="1.0" encoding="UTF-8"?>
<package xmlns="http://www.idpf.org/2007/opf" version="3.0" unique-identifier="book-id">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:title>Stage 0 EPUB Probe</dc:title></metadata>
  <manifest>
    <item id="nav" href="nav.xhtml" media-type="application/xhtml+xml" properties="nav"/>
    <item id="chapter" href="text/chapter.xhtml" media-type="application/xhtml+xml"/>
  </manifest>
  <spine><itemref idref="chapter"/></spine>
</package>"#,
        deflated,
    )?;
    write_entry(
        &mut writer,
        "EPUB/nav.xhtml",
        br#"<!doctype html><html><body><nav><ol><li><a href="text/chapter.xhtml">Chapter</a></li></ol></nav></body></html>"#,
        deflated,
    )?;
    write_entry(
        &mut writer,
        "EPUB/text/chapter.xhtml",
        br#"<!doctype html><html><body>
<section><h1>Measured reading</h1><p onclick="steal()">First normalized paragraph.</p>
<p><a href="javascript:steal()">Unsafe link</a> remains plain text.</p>
<img src="cover.jpg" onerror="steal()"><script>steal()</script></section>
</body></html>"#,
        deflated,
    )?;

    let cursor = writer.finish()?;
    Ok(cursor.into_inner())
}

fn write_entry(
    writer: &mut ZipWriter<Cursor<Vec<u8>>>,
    path: &str,
    bytes: &[u8],
    options: SimpleFileOptions,
) -> Result<(), EpubProbeError> {
    writer.start_file(path, options)?;
    writer.write_all(bytes)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_parses_package_spine_navigation_and_content() -> Result<(), EpubProbeError> {
        let report = run_epub_probe()?;

        assert_eq!(
            report,
            EpubProbeReport {
                entry_count: 5,
                title: "Stage 0 EPUB Probe".to_owned(),
                spine_item_count: 1,
                navigation_item_count: 1,
                paragraph_count: 2,
                active_content_removed: true,
            }
        );
        Ok(())
    }

    #[test]
    fn inspect_rejects_path_traversal_before_extraction() -> Result<(), EpubProbeError> {
        let archive = build_archive_with_entry("../escape.xhtml", b"escape")?;

        let result = inspect_epub(&archive, EpubLimits::s1());

        assert!(matches!(result, Err(EpubProbeError::UnsafePath(_))));
        Ok(())
    }

    #[test]
    fn inspect_rejects_excessive_compression_ratio() -> Result<(), EpubProbeError> {
        let repeated = vec![b'a'; 64 * 1024];
        let archive = build_archive_with_entry("EPUB/bomb.txt", &repeated)?;
        let mut limits = EpubLimits::s1();
        limits.compression_ratio = 2;

        let result = inspect_epub(&archive, limits);

        assert!(matches!(
            result,
            Err(EpubProbeError::CompressionRatioExceeded { .. })
        ));
        Ok(())
    }

    #[test]
    fn inspect_rejects_entry_count_before_parsing() -> Result<(), EpubProbeError> {
        let archive = build_fixture_epub()?;
        let mut limits = EpubLimits::s1();
        limits.entries = 4;

        let result = inspect_epub(&archive, limits);

        assert!(matches!(
            result,
            Err(EpubProbeError::TooManyEntries {
                actual: 5,
                limit: 4
            })
        ));
        Ok(())
    }

    #[test]
    fn package_xml_rejects_doctype_declarations() {
        let package = br#"<!DOCTYPE package [<!ENTITY secret SYSTEM "file:///etc/passwd">]>
<package><metadata><title>&secret;</title></metadata></package>"#;

        let result = parse_package(package);

        assert!(matches!(result, Err(EpubProbeError::Xml(_))));
    }

    #[test]
    fn inspect_rejects_duplicate_archive_paths() -> Result<(), EpubProbeError> {
        let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
        let stored = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
        let deflated = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
        write_entry(&mut writer, "mimetype", EPUB_MIMETYPE, stored)?;
        write_entry(&mut writer, "EPUB\\duplicate.xhtml", b"first", deflated)?;
        write_entry(&mut writer, "EPUB/duplicate.xhtml", b"second", deflated)?;
        let archive = writer.finish()?.into_inner();

        let result = inspect_epub(&archive, EpubLimits::s1());

        assert!(matches!(result, Err(EpubProbeError::DuplicatePath(_))));
        Ok(())
    }

    fn build_archive_with_entry(path: &str, content: &[u8]) -> Result<Vec<u8>, EpubProbeError> {
        let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
        let stored = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
        let deflated = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
        write_entry(&mut writer, "mimetype", EPUB_MIMETYPE, stored)?;
        write_entry(&mut writer, path, content, deflated)?;
        let cursor = writer.finish()?;
        Ok(cursor.into_inner())
    }
}
