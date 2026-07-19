# EPUB golden corpus

`supported.expected.json` фиксирует projection для синтетического EPUB,
который детерминированно собирается функцией `fixture_epub` в
`crates/lumi-core/src/epub.rs`. Все XML/XHTML/image bytes находятся рядом с
тестом как исходный код fixture и не имеют внешних лицензионных ограничений.
Corpus покрывает EPUB 3 navigation, spine, image resource, active-content
sanitization, traversal, DOCTYPE, encryption и большой reflowable document.
Дополнительные синтетические варианты в `epub.rs` проверяют безопасное
восстановление неканонического расположения/сжатия `mimetype`, XHTML
self-closing non-void elements, SVG image wrappers, сложный SVG placeholder и
пропуск отдельных пустых spine items. Реальные книги для этих классов не входят
в corpus и не требуются в CI.
