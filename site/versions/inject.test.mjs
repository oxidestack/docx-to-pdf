// inject.mjs on a site of two releases (site/README.md, "Versions").
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { mkdtempSync, mkdirSync, writeFileSync, readFileSync, rmSync } from 'node:fs'
import { join } from 'node:path'
import { tmpdir } from 'node:os'
import { main, inject } from './inject.mjs'

const PAGE = '<html><head><title>t</title></head><body></body></html>'

/** A site as build.sh leaves it: 1.1 at the root and under 1.1/, 1.0 under 1.0/ without an upgrade guide. */
function site() {
  const dir = mkdtempSync(join(tmpdir(), 'site-versions-'))
  for (const page of ['index.html', 'installation/index.html', 'upgrading/index.html', '1.1/index.html',
    '1.1/upgrading/index.html', '1.0/index.html', '1.0/installation/index.html']) {
    mkdirSync(join(dir, page, '..'), { recursive: true })
    writeFileSync(join(dir, page), PAGE)
  }
  return dir
}

test('every page says its version, and only the root is indexed', () => {
  const dir = site()
  try {
    main([dir, '/docx-to-pdf', 'https://github.com/oxidestack/docx-to-pdf', '1.1', '1.0'])
    const page = (p) => readFileSync(join(dir, p), 'utf8')
    for (const p of ['index.html', 'installation/index.html']) {
      assert.match(page(p), /<meta name="docs-version" content="1.1">/)
      assert.doesNotMatch(page(p), /noindex/)
    }
    assert.match(page('1.1/index.html'), /content="1.1"><meta name="robots" content="noindex">/)
    assert.match(page('1.0/installation/index.html'), /content="1.0"><meta name="robots" content="noindex">/)
    assert.match(page('1.0/index.html'), /<script type="module" src="\/docx-to-pdf\/versions.js"><\/script><\/head>/)
    readFileSync(join(dir, 'versions.js'))
  } finally {
    rmSync(dir, { recursive: true })
  }
})

test('versions.json lists the latest first, at the root, with whether each has an upgrade guide', () => {
  const dir = site()
  try {
    main([dir, '/docx-to-pdf', 'https://github.com/oxidestack/docx-to-pdf', '1.1', '1.0'])
    assert.deepEqual(JSON.parse(readFileSync(join(dir, 'versions.json'), 'utf8')), {
      latest: '1.1',
      changelog: 'https://github.com/oxidestack/docx-to-pdf/blob/main/CHANGELOG.md',
      versions: [
        { version: '1.1', path: '/docx-to-pdf/', upgrading: true },
        { version: '1.0', path: '/docx-to-pdf/1.0/', upgrading: false },
      ],
    })
  } finally {
    rmSync(dir, { recursive: true })
  }
})

test('a page is injected once', () => {
  const once = inject(PAGE, { version: '1.0', root: '/docx-to-pdf', archived: true })
  assert.equal(inject(once, { version: '1.0', root: '/docx-to-pdf', archived: true }), once)
})
