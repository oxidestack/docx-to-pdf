# Feature documents

The corpus's own DOCX documents, written by this project and licensed as the rest of the repository (Apache License
2.0). `generate.py` assembles each one part by part from WordprocessingML written in the script, with pictures drawn
by a seeded generator, so every byte is the project's own:

```sh
python3 generate.py            # rewrites the documents here and prints each one's sha256
python3 generate.py OUT        # writes them to OUT instead
```

It needs Python 3 and Pillow. The ZIP entries carry a fixed date and a fixed order, so the same Pillow and zlib give
the same bytes; `../../manifest.tsv` pins each document's sha256, and a run with other versions shows up there as a
changed hash. A changed document needs moalhaddar/docx-to-pdf's PDF recorded again (`make corpus-record ONLY=`) and
its rendering baseline approved again (`../../README.md`).

| Document | What it exercises |
| --- | --- |
| `arabic-lists` | right-to-left Arabic paragraphs in numbered lists (decimal and Arabic letters), embedded left-to-right words and numbers |
| `bidi-worksheet` | right-to-left paragraphs in a left-to-right worksheet, content controls in the footer (text, date), PAGE and NUMPAGES in the header, a title page, a DrawingML text box of relative width with its VML fallback, a GIF, a two-level list |
| `case-study-pictures` | two baseline 4:4:4 JPEGs (888 × 552, a 1.47 MB bitmap, and 320 × 200), two PNGs, a header picture, two sections, two columns with a column break, a DrawingML text box with a pattern fill, a VML text box, a frame, six tables, PAGE and NUMPAGES |
| `chart-ole-textbox` | a line chart with its embedded workbook, an embedded spreadsheet (OLE object with a PNG preview), a DrawingML text box with its VML fallback, a PNG |
| `chinese-spec` | a Chinese specification: a revision table, a floating table, three-level numbered headings, STYLEREF in the header, VML pictures, a line grid |
| `citations-comments` | CITATION fields in citation content controls, a BIBLIOGRAPHY field in a bibliography content control with its sources in custom XML, overlapping comments with commentsExtended and people parts, cell shading |
| `column-section-breaks` | a document that starts with a continuous two-column section, column balancing at continuous breaks, a page break inside columns, a column separator line, a right-to-left section, A6 landscape |
| `content-control-form` | a form: text, date, drop-down and check-box content controls with placeholder text, anchored PNG and GIF pictures, an external hyperlink, paragraph borders |
| `drop-cap-dashes` | drop caps (`framePr` dropCap) that are a hyphen, an en dash, an em dash and a figure dash |
| `endnote-continuation` | an endnote longer than a short page (612 × 283 pt), continued on the next page under the continuation separator |
| `equation-captions` | Office Math display equations (an n-ary sum with limits, a binomial, fractions, factorials), inline equations, SEQ caption fields above and below |
| `floating-table-math` | a floating table (`tblpPr`) holding Office Math, a caption, a footer PAGE field, a title page |
| `floating-tables-columns` | two floating tables, one anchored to the page and one in a two-column section, anchored DrawingML text boxes, an anchored PNG, numbering, first, default and even headers and footers |
| `footnotes-per-page` | one footnote on each of six pages, numbering restarted on each page, right-to-left paragraphs and footnotes |
| `full-width-line` | an inline DrawingML rectangle (VML fallback) wider than the text area after a bold title, in a paragraph of line spacing 0 at least, beside a picture anchored in the header down the page's left edge and wrapped through; plus a floating table, an external hyperlink, a bulleted list. It hangs moalhaddar/docx-to-pdf's LibreOffice 7.3 (no answer in 300 s, LibreOffice at 100 % CPU), so `../../divergences.tsv` lists it as `converts` |
| `japanese-vertical` | Japanese text in a vertical (`tbRl`) section on a line grid, A4 landscape |
| `legacy-form-fields` | legacy form fields (FORMTEXT, FORMCHECKBOX, FORMDROPDOWN), an ASK field, bookmarks and REF fields, comments, a three-level clause numbering with shaded headings, a frame, caps |
| `letterhead-sections` | six sections, a title page with a VML text box and a WMF picture in its header, VML shapes in a header, STYLEREF and PAGE fields, a page-number restart, a comment, small caps, decimal and left tab stops, German text |
| `long-footnotes` | eight footnotes, the long ones split across pages, a Roman and decimal list, shading |
| `long-toc-form` | a German consent form: content-control placeholders in the header, a table of contents of 40 entries with PAGEREF hyperlinks, DrawingML text boxes, caps, borders, numbered headings, "Seite X von Y" |
| `math-paper-damaged-table` | a paper with inline and display Office Math, numbered sections and references, first, odd and even headers, a bordered abstract, and a damaged table (a grid narrower than its rows, a cell without a paragraph, a span past the grid, no widths) |
| `multilevel-numbering` | three-level legal numbering (1., 1.1., 1.1.1.), a lettered list, caps and small caps, paragraph borders, a title page |
| `page-background-gradient` | a page background (`w:background`) with a VML radial gradient of five stops |
| `page-x-of-y` | "Page X of Y" (PAGE and NUMPAGES) in the footer, a table of contents, two sections, caps |
| `relative-width-lines` | VML horizontal rules of 100, 75, 50, 25 and 10 % of the column, aligned left, centre and right, in columns, in a table and in a text box; four sections, A5 landscape |
| `smartart-pictures` | SmartArt (data, layout, style, colours and the cached drawing) with pictures in its boxes, four 4:2:0 JPEGs at odd sizes (61 × 71, 195 × 129, 129 × 195, 97 × 61; qualities 75 and 80), table shading, a bulleted list, paragraph borders |
| `smartart-sections` | SmartArt in two sections, a header picture, a footer PAGE field, a numbered list |
| `strict-ooxml` | ISO/IEC 29500 Strict namespaces and relationship types: a header, a chart, SmartArt, Office Math, a GIF |
| `table-formulas-unicode` | table formula fields naming bookmarks in Chinese, Arabic and English, a sum and a faulty expression, REF fields, first, default and even headers and footers |
| `table-vertical-text` | a table with vertically merged cells of bottom-to-top and top-to-bottom text, a row that cannot split, coloured borders, cell shading |
| `theme-fonts` | theme fonts (major and minor), Chinese and Hebrew text, a patterned paragraph shading in theme colours, a custom style |
| `toc-field-f` | a table of contents with the `\f` switch and TC fields, headings of three levels |
| `toc-section-margins` | a table of contents, six sections of different margins, a two-column section with a column break, a footnote, Roman then decimal page numbers, numbered headings |
| `toc-styleref` | a table of contents, STYLEREF in the header, title pages, two sections with their own headers |
| `tracked-changes-sections` | ten sections (next page and continuous, two of two columns), tracked insertions and deletions by two authors, numbering, frames, page-number restarts, tabs, preserved spaces |
| `tracked-table-cells` | tracked cell insertions and deletions in a calendar table, a tracked paragraph, two landscape sections, a title page, Hungarian text |
| `tracked-table-columns` | tracked inserted and deleted empty table columns (cellIns and cellDel, tracked paragraph marks), a right-to-left paragraph, a drop-down content control, tracked runs |
