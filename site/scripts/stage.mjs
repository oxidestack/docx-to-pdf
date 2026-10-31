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
import { createHash } from 'node:crypto'
import { TRANSLATIONS, README } from '../locales.mjs'

const here = dirname(fileURLToPath(import.meta.url))
const REPO = join(here, '..', '..')
const DOCS = join(here, '..', 'src', 'content', 'docs')

/** The site's root path: the latest release's documentation, and what the README writes the site's address with. */
const ROOT = '/docx-to-pdf'
/** The site's base path (astro.config.mjs `base`): every internal link carries it. A release's own is its version's. */
const BASE = process.env.SITE_BASE || ROOT
const GITHUB = 'https://github.com/oxidestack/docx-to-pdf'
/** The git ref a link to a file of the repository names: the release's tag for a release's documentation. */
const REF = process.env.SITE_REF || 'main'
/** The site's origin (astro.config.mjs `site`). */
const SITE = 'https://oxidestack.github.io'

/**
 * The staged files: repository path → the page's slug ('' is the site's home page) and language ('' is English).
 * A translation of README.md is its language's README beside it (locales.mjs, README.ru.md); a translation of a guide
 * page is docs/i18n/<language>/guide/ with the page's name, docs/i18n/ru/guide/installation.md. It is staged as the
 * same page under its language, and a translation of no page fails the staging.
 */
function pages() {
  const map = new Map([['README.md', { slug: '', locale: '' }]])
  for (const file of readdirSync(join(REPO, 'docs', 'guide')).sort()) {
    if (file.endsWith('.md')) map.set(`docs/guide/${file}`, { slug: basename(file, '.md'), locale: '' })
  }
  for (const locale of TRANSLATIONS) {
    for (const [original, { slug }] of [...map].filter(([, page]) => page.locale === '')) {
      const translation = original === 'README.md' ? README[locale] : `docs/i18n/${locale}/${original.replace(/^docs\//, '')}`
      if (existsSync(join(REPO, translation))) map.set(translation, { slug, locale })
    }
    const dir = join(REPO, 'docs', 'i18n', locale)
    for (const file of existsSync(dir) ? readdirSync(dir, { recursive: true }) : []) {
      const path = `docs/i18n/${locale}/${file}`
      if (path.endsWith('.md') && !map.has(path)) throw new Error(`${path} translates no page: README.md and docs/guide/*.md are`)
    }
  }
  return map
}

/** The original of a translation's repository path: README.ru.md is README.md, docs/i18n/ru/guide/api.md is
 * docs/guide/api.md. */
const original = (path) => (Object.values(README).includes(path) ? 'README.md' : path.replace(/^docs\/i18n\/[^/]+\//, 'docs/'))

const LANGUAGES = /<!-- languages:begin -->[\s\S]*?<!-- languages:end -->\n*/

/**
 * The languages line of an original page, `<!-- languages:begin -->` to `<!-- languages:end -->`: on GitHub it links
 * each translation, as the CNCF's code of conduct links its own; on the site the language picker does, so the
 * staging takes it out. It must link every translation the page has, and only those.
 */
function languages(source, from, staged) {
  const block = LANGUAGES.exec(source)
  const translations = [...staged]
    .filter(([path, page]) => page.locale !== '' && original(path) === from)
    .map(([path]) => path)
  if (staged.get(from).locale === '') {
    const linked = block
      ? [...block[0].matchAll(/\]\(([^)#\s]+)/g)].map((m) => posix.normalize(posix.join(posix.dirname(from), m[1])))
      : []
    const missing = translations.filter((path) => !linked.includes(path))
    const extra = linked.filter((path) => !translations.includes(path))
    if (missing.length || extra.length) {
      throw new Error(
        `${from}: its languages line must link its translations, ${translations.join(', ') || 'none'}; ` +
          `it misses ${missing.join(', ') || 'none'} and links ${extra.join(', ') || 'nothing else'} besides`,
      )
    }
  }
  return source.replace(LANGUAGES, '')
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

/** The staged pages' directory, as a repository path: every page is staged flat into it, a translation into its
 * language's directory. */
const STAGED_DIR = 'site/src/content/docs'

/** A page's path on the site, in the language `locale`. */
const sitePath = (slug, locale) => `${BASE}/${locale ? `${locale}/` : ''}${slug ? `${slug}/` : ''}`

/**
 * One link target, as seen from the repository file `from`, a page in the language `locale`; `image` when it is an
 * image's source. A link to a page, original or translated, is to that page in the linking page's language: a
 * translation linking an untranslated page links its English fallback there.
 */
function retarget(href, from, staged, image, locale) {
  // The site's own address, as the README writes it to link a page GitHub does not carry: a path under the base on
  // the site, so the validator checks the page and the anchor it names.
  if (href.startsWith(`${SITE}${ROOT}/`)) {
    return BASE + (locale ? `/${locale}` : '') + href.slice(SITE.length + ROOT.length)
  }
  if (/^([a-z][a-z0-9+.-]*:|#|\/)/i.test(href)) return href
  const [path, hash = ''] = href.split('#')
  const anchor = hash ? `#${hash}` : ''
  const target = posix.normalize(posix.join(posix.dirname(from), path))
  if (staged.has(target)) return sitePath(staged.get(target).slug, locale) + anchor
  if (target.startsWith('..')) throw new Error(`${from}: the link ${href} leaves the repository`)
  if (!existsSync(join(REPO, target))) throw new Error(`${from}: the link ${href} names no file of the repository`)
  if (image) {
    if (!target.startsWith('site/src/')) throw new Error(`${from}: the image ${href} is not under site/src/, so the site cannot show it`)
    return posix.relative(locale ? `${STAGED_DIR}/${locale}` : STAGED_DIR, target) + anchor
  }
  const kind = path.endsWith('/') ? 'tree' : 'blob'
  return `${GITHUB}/${kind}/${REF}/${target}${anchor}`
}

function retargetLinks(source, from, staged, locale) {
  return overProse(
    source,
    (text) =>
      text.replace(/(!?\[[^\]]*\]\()([^)\s]+)(\))/g, (_, open, href, close) =>
        open + retarget(href, from, staged, open.startsWith('!'), locale) + close,
      ),
    { spans: true },
  )
}

/** A string as YAML, quoted the one way that always holds. */
const yaml = (s) => `"${s.replace(/\\/g, '\\\\').replace(/"/g, '\\"')}"`

/**
 * A translation's record of the original it translates: `<!-- translation of README.md: 1a2b3c4d5e6f -->`, the first
 * 12 hex digits of the original's SHA-256 when it was translated (`sha256sum README.md | cut -c1-12`). Every
 * translation has one; when the original's digest is another, the original changed since, and the translated page
 * says it may be out of date and links the original.
 */
const SOURCE = /<!-- translation of (\S+): ([0-9a-f]{12}) -->\n*/

const digest = (text) => createHash('sha256').update(text).digest('hex').slice(0, 12)

/** "This translation may be out of date" in each translated language, with the link to the original. */
const OUTDATED = {
  ru: (href) => `Перевод может быть устаревшим: английский оригинал изменился после него. <a href="${href}">Читать оригинал</a>.`,
  zh: (href) => `此翻译可能已过时：英文原文在翻译之后有过修改。<a href="${href}">阅读原文</a>。`,
  es: (href) => `Esta traducción puede estar desactualizada: el original en inglés ha cambiado desde entonces. <a href="${href}">Leer el original</a>.`,
}

/** The translation's source without its record, and whether its original changed since; the original as it is. */
function freshness(source, from, locale) {
  if (!locale) return { source, outdated: false }
  const record = SOURCE.exec(source)
  const of = original(from)
  if (!record || record[1] !== of) {
    throw new Error(`${from}: a translation records its original, <!-- translation of ${of}: <its sha256, 12 hex digits> --> (site/README.md, "Languages")`)
  }
  const now = digest(readFileSync(join(REPO, of)))
  return { source: source.replace(SOURCE, ''), outdated: record[2] !== now }
}

/** The translations whose original changed since, for the build's log. */
const outdated = []

function stage(from, { slug, locale }, staged) {
  const fresh = freshness(readFileSync(join(REPO, from), 'utf8'), from, locale)
  const { text, body } = title(languages(fresh.source, from, staged), from)
  const content = overProse(retargetLinks(body, from, staged, locale), escapeAngles)
  const head = ['---', `title: ${yaml(text)}`, `editUrl: ${yaml(`${GITHUB}/edit/main/${from}`)}`]
  if (fresh.outdated) {
    outdated.push(from)
    head.push('banner:', `  content: ${yaml(OUTDATED[locale](sitePath(slug, '')))}`)
  }
  head.push('---', '')
  const target = join(DOCS, locale, `${slug || 'index'}.md`)
  mkdirSync(dirname(target), { recursive: true })
  writeFileSync(target, `${head.join('\n')}${content.trimEnd()}\n`)
}

function main() {
  rmSync(DOCS, { recursive: true, force: true })
  const staged = pages()
  for (const [from, page] of staged) stage(from, page, staged)
  const translated = [...staged.values()].filter((page) => page.locale !== '').length
  console.log(`staged: ${staged.size - translated} pages (README.md and docs/guide/), ${translated} translations (README.<language>.md and docs/i18n/)`)
  for (const from of outdated) console.log(`staged: ${from} may be out of date: ${original(from)} changed since it was translated`)
}

if (existsSync(join(REPO, 'Cargo.toml')) && existsSync(join(REPO, 'docs', 'guide'))) main()
else throw new Error(`the repository is not where this script expects it: ${REPO}`)
