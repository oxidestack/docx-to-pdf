# Writes tests/scenarios/fixtures/comment.docx, a minimal hand-made DOCX with one comment (word/comments.xml, a
# commentRangeStart/End and a commentReference): Writer builds its comment view while loading one. Usage:
#   python3 -I tests/scenarios/fixtures/make_comment_docx.py tests/scenarios/fixtures/comment.docx 1
# The second argument 0 writes the same paragraph without the comment. Fixed zip timestamps: same bytes each run.
import sys
import zipfile

W = 'xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"'
R = 'xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"'
with_comment = sys.argv[2] == "1"
files = {
    "[Content_Types].xml": '<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>'
    + (
        '<Override PartName="/word/comments.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.comments+xml"/>'
        if with_comment
        else ""
    )
    + "</Types>",
    "_rels/.rels": '<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>',
    "word/_rels/document.xml.rels": '<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">'
    + (
        '<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/comments" Target="comments.xml"/>'
        if with_comment
        else ""
    )
    + "</Relationships>",
}
if with_comment:
    body = '<w:p><w:r><w:t xml:space="preserve">A paragraph with </w:t></w:r><w:commentRangeStart w:id="0"/><w:r><w:t>one comment</w:t></w:r><w:commentRangeEnd w:id="0"/><w:r><w:commentReference w:id="0"/></w:r><w:r><w:t>.</w:t></w:r></w:p>'
    files["word/comments.xml"] = (
        f'<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:comments {W}><w:comment w:id="0" w:author="Reviewer" w:date="2026-10-21T00:00:00Z" w:initials="R"><w:p><w:r><w:t>The comment.</w:t></w:r></w:p></w:comment></w:comments>'
    )
else:
    body = "<w:p><w:r><w:t>A paragraph with one comment.</w:t></w:r></w:p>"
files["word/document.xml"] = (
    f'<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:document {W} {R}><w:body>{body}<w:sectPr><w:pgSz w:w="11906" w:h="16838"/><w:pgMar w:top="1440" w:right="1440" w:bottom="1440" w:left="1440" w:header="708" w:footer="708" w:gutter="0"/></w:sectPr></w:body></w:document>'
)
with zipfile.ZipFile(sys.argv[1], "w", zipfile.ZIP_DEFLATED) as z:
    for name in ["[Content_Types].xml", "_rels/.rels", "word/_rels/document.xml.rels", "word/document.xml"] + (
        ["word/comments.xml"] if with_comment else []
    ):
        zi = zipfile.ZipInfo(name, date_time=(2026, 10, 21, 0, 0, 0))
        zi.compress_type = zipfile.ZIP_DEFLATED
        z.writestr(zi, files[name])
