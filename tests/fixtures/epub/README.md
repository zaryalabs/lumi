# EPUB golden corpus

`supported.expected.json` фиксирует projection для синтетического EPUB,
который детерминированно собирается функцией `fixture_epub` в
`crates/lumi-core/src/epub.rs`. Все XML/XHTML/image bytes находятся рядом с
тестом как исходный код fixture и не имеют внешних лицензионных ограничений.
Corpus покрывает EPUB 3 navigation, spine, image resource, active-content
sanitization, traversal, DOCTYPE, encryption и большой reflowable document.
