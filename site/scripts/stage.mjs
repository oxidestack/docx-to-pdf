// Stage the repository's user documentation into the site's content collection.
//
// README.md and docs/guide/*.md are the documentation, the files a reader of the repository sees on GitHub; this
// site is a second way to read the same files and never a second copy of them. So the staged tree,
// src/content/docs/, is build output: deleted and rewritten on every build, ignored by git, and an edit made in it is
// gone on the next `npm run build`.
//
// The transformation is the three things a file written for a repository cannot do on a site, and nothing else:
//
//   1. A title. Starlight wants it in frontmatter; a Markdown file states it as its first `# ` heading, which is
//      lifted out so the page has one <h1>, not two.
//   2. Links that resolve. `[Configuration](configuration.md#server)` is right on GitHub and a 404 here, where the
//      page is /docx-to-pdf/configuration/. A link to a staged file becomes its page; a link to any other file of the
//      repository becomes a link to that file on GitHub, since the site does not carry it. An image is the exception:
//      a GitHub page is no image, so an image must be one of the site's own sources (site/src/…), linked relative
//      to the staged page so the build bundles it; any other image fails the staging.
//      A link to the site's own address (the README's to the Benchmarks page, which only the site has) becomes a
//      path under the base, so the build checks it like any other.
//   3. Angle brackets that survive: outside code a renderer reads `<name>` as an HTML tag and drops it, so it is
//      escaped there. Code, fenced or inline, is left exactly as written, so an example copied from a page still runs.

import { readFileSync, writeFileSync, mkdirSync, rmSync, readdirSync, existsSync } from 'node:fs'
import { dirname, join, basename, posix } from 'node:path'
import { fileURLToPath } from 'node:url'

const here = dirname(fileURLToPath(import.meta.url))
const REPO = join(here, '..', '..')
const DOCS = join(here, '..', 'src', 'content', 'docs')

/** The site's base path (astro.config.mjs `base`): every internal link carries it. */
const BASE = '/docx-to-pdf'
const GITHUB = 'https://github.com/oxidestack/docx-to-pdf'
/** The site's origin (astro.config.mjs `site`). */
const SITE = 'https://oxidestack.github.io'

/** The staged files: repository path → page slug ('' is the site's home page). */
function pages() {
  const map = new Map([['README.md', '']])
  for (const file of readdirSync(join(REPO, 'docs', 'guide')).sort()) {
    if (file.endsWith('.md')) map.set(`docs/guide/${file}`, basename(file, '.md'))
  }
  return map
}

/** The first `# ` heading as the title, and the body without it. */
function title(source, from) {
  const lines = source.split('\n')
  const at = lines.findIndex((l) => l.startsWith('# '))
  if (at === -1) throw new Error(`${from} has no '# ' heading, so it has no title to stage`)
  const text = lines[at].slice(2).trim()
  lines.splice(at, 1)
  while (lines.length && lines[0].trim() === '') lines.shift()
  return { text, body: lines.join('\n') }
}

/** The source split into code (fenced blocks) and prose, so a rewrite touches prose alone. */
function segments(source) {
  const out = []
  const fence = /^(\s*)(```+|~~~+)/
  let prose = []
  let inFence = null
  for (const line of source.split('\n')) {
    const m = fence.exec(line)
    if (inFence) {
      out.push({ code: true, text: line })
      if (m && m[2].startsWith(inFence)) inFence = null
      continue
    }
    if (m) {
      if (prose.length) out.push({ code: false, text: prose.join('\n') })
      prose = []
      out.push({ code: true, text: line })
      inFence = m[2]
      continue
    }
    prose.push(line)
  }
  if (prose.length) out.push({ code: false, text: prose.join('\n') })
  return out
}

/**
 * `f` applied to the prose outside fenced blocks. `spans: true` hands it inline code too: a link whose text is code,
 * [`NOTICE`](NOTICE), must reach the link rewrite whole; escaping angle brackets wants inline code left alone.
 */
function overProse(source, f, { spans = false } = {}) {
  return segments(source)
    .map((s) => {
      if (s.code) return s.text
      if (spans) return f(s.text)
      return s.text
        .split(/(`+[^`]*`+)/g)
        .map((part, i) => (i % 2 === 1 ? part : f(part)))
        .join('')
    })
    .join('\n')
}

/**
 * `<name>` outside code is a tag to a renderer and a placeholder to a reader. An autolink stays a link, and an HTML
 * comment (a markdownlint directive) stays a comment, unseen on the page as on GitHub.
 */
function escapeAngles(text) {
  return text.replace(/<(?!https?:\/\/|!--)/g, '&lt;')
}

/** The staged pages' directory, as a repository path: every page is staged flat into it. */
const STAGED_DIR = 'site/src/content/docs'

/** One link target, as seen from the repository file `from`; `image` when it is an image's source. */
function retarget(href, from, staged, image) {
  // The site's own address, as the README writes it to link a page GitHub does not carry: a path under the base on
  // the site, so the validator checks the page and the anchor it names.
  if (href.startsWith(`${SITE}${BASE}/`)) return href.slice(SITE.length)
  if (/^([a-z][a-z0-9+.-]*:|#|\/)/i.test(href)) return href
  const [path, hash = ''] = href.split('#')
  const anchor = hash ? `#${hash}` : ''
  const target = posix.normalize(posix.join(posix.dirname(from), path))
  if (staged.has(target)) {
    const slug = staged.get(target)
    return `${BASE}/${slug ? `${slug}/` : ''}${anchor}`
  }
  if (target.startsWith('..')) throw new Error(`${from}: the link ${href} leaves the repository`)
  if (!existsSync(join(REPO, target))) throw new Error(`${from}: the link ${href} names no file of the repository`)
  if (image) {
    if (!target.startsWith('site/src/')) throw new Error(`${from}: the image ${href} is not under site/src/, so the site cannot show it`)
    return posix.relative(STAGED_DIR, target) + anchor
  }
  const kind = path.endsWith('/') ? 'tree' : 'blob'
  return `${GITHUB}/${kind}/main/${target}${anchor}`
}

function retargetLinks(source, from, staged) {
  return overProse(
    source,
    (text) =>
      text.replace(/(!?\[[^\]]*\]\()([^)\s]+)(\))/g, (_, open, href, close) =>
        open + retarget(href, from, staged, open.startsWith('!')) + close,
      ),
    { spans: true },
  )
}

/** A string as YAML, quoted the one way that always holds. */
const yaml = (s) => `"${s.replace(/\\/g, '\\\\').replace(/"/g, '\\"')}"`

function stage(from, slug, staged) {
  const { text, body } = title(readFileSync(join(REPO, from), 'utf8'), from)
  const content = overProse(retargetLinks(body, from, staged), escapeAngles)
  const head = ['---', `title: ${yaml(text)}`, `editUrl: ${yaml(`${GITHUB}/edit/main/${from}`)}`, '---', '']
  const target = join(DOCS, `${slug || 'index'}.md`)
  mkdirSync(dirname(target), { recursive: true })
  writeFileSync(target, `${head.join('\n')}${content.trimEnd()}\n`)
}

function main() {
  rmSync(DOCS, { recursive: true, force: true })
  const staged = pages()
  for (const [from, slug] of staged) stage(from, slug, staged)
  console.log(`staged: ${staged.size} pages (README.md and docs/guide/)`)
}

if (existsSync(join(REPO, 'Cargo.toml')) && existsSync(join(REPO, 'docs', 'guide'))) main()
else throw new Error(`the repository is not where this script expects it: ${REPO}`)
