#!/usr/bin/env python3
"""Generate deterministic, source-controlled PDF import fixtures."""

from __future__ import annotations

import os
from pathlib import Path

from reportlab.lib import colors
from reportlab.lib.pagesizes import A4, landscape
from reportlab.pdfbase import pdfmetrics
from reportlab.pdfbase.ttfonts import TTFont
from reportlab.pdfgen import canvas


ROOT = Path(__file__).resolve().parents[1]
OUTPUT = ROOT / "tests" / "fixtures" / "pdf" / "text-layer.pdf"
FONT_CANDIDATES = (
    Path(os.environ.get("LUMI_PDF_FIXTURE_FONT", "")),
    Path("/System/Library/Fonts/Supplemental/Arial Unicode.ttf"),
    Path("/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf"),
)


def fixture_font() -> str:
    for candidate in FONT_CANDIDATES:
        if candidate.is_file():
            pdfmetrics.registerFont(TTFont("LumiFixtureSans", candidate))
            return "LumiFixtureSans"
    return "Helvetica"


def draw_header(document: canvas.Canvas, font: str, title: str, page: str) -> None:
    document.setFillColor(colors.HexColor("#24483B"))
    document.setFont(font, 11)
    document.drawString(48, document._pagesize[1] - 42, "LUMI · PDF FIXTURE")
    document.setFillColor(colors.HexColor("#222722"))
    document.setFont(font, 22)
    document.drawString(48, document._pagesize[1] - 80, title)
    document.setFillColor(colors.HexColor("#68716A"))
    document.setFont(font, 9)
    document.drawRightString(document._pagesize[0] - 48, 30, page)


def generate() -> None:
    OUTPUT.parent.mkdir(parents=True, exist_ok=True)
    font = fixture_font()
    document = canvas.Canvas(
        str(OUTPUT),
        pagesize=A4,
        pageCompression=1,
        invariant=1,
    )
    document.setTitle("Lumi PDF text layer fixture")
    document.setAuthor("Lumi tests")
    document.setSubject("PDF import, geometry and text-layer verification")

    draw_header(document, font, "Слой текста и геометрия", "1 / 2")
    document.bookmarkPage("text-layer")
    document.addOutlineEntry("Text layer", "text-layer", level=0)
    document.setFont(font, 13)
    lines = (
        "Этот PDF содержит нативный текстовый слой.",
        "Selection должна совпадать с видимыми строками.",
        "Anchors сохраняются в координатах физической страницы.",
        "English fallback: searchable text remains selectable.",
    )
    y = A4[1] - 128
    for line in lines:
        document.drawString(62, y, line)
        y -= 27
    document.setStrokeColor(colors.HexColor("#AEB9B1"))
    document.setFillColor(colors.HexColor("#EDF2EE"))
    document.roundRect(58, y - 94, A4[0] - 116, 84, 8, stroke=1, fill=1)
    document.setFillColor(colors.HexColor("#24483B"))
    document.setFont(font, 11)
    document.drawString(76, y - 40, "Expected: two pages, native text, portrait + landscape.")
    document.drawString(76, y - 63, "The source is deterministic and has no scripts or attachments.")
    document.linkAbsolute(
        "Go to geometry page",
        "geometry",
        Rect=(62, 88, 230, 110),
        thickness=0,
    )
    document.setFillColor(colors.HexColor("#2F6E59"))
    document.drawString(62, 94, "Открыть страницу геометрии →")
    document.showPage()

    document.setPageSize(landscape(A4))
    draw_header(document, font, "Landscape page and links", "2 / 2")
    document.bookmarkPage("geometry")
    document.addOutlineEntry("Geometry", "geometry", level=0)
    document.setFont(font, 12)
    document.setFillColor(colors.HexColor("#222722"))
    document.drawString(54, landscape(A4)[1] - 124, "Wide page verifies per-page dimensions and lazy rendering.")
    columns = (54, 250, 448, 646)
    labels = ("Page", "Width", "Height", "Text layer")
    values = ("2", "841.89 pt", "595.28 pt", "native")
    for x, label, value in zip(columns, labels, values, strict=True):
        document.setFillColor(colors.HexColor("#E6EEE8"))
        document.roundRect(x, 344, 166, 88, 6, stroke=0, fill=1)
        document.setFillColor(colors.HexColor("#68716A"))
        document.setFont(font, 9)
        document.drawString(x + 14, 402, label.upper())
        document.setFillColor(colors.HexColor("#222722"))
        document.setFont(font, 14)
        document.drawString(x + 14, 370, value)
    document.setFillColor(colors.HexColor("#2F6E59"))
    document.setFont(font, 11)
    document.drawString(54, 88, "External test link: https://example.com/")
    document.linkURL(
        "https://example.com/",
        (190, 82, 330, 104),
        relative=0,
        thickness=0,
    )
    document.save()


if __name__ == "__main__":
    generate()
