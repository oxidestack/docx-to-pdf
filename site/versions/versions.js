// The version picker and the banner of an older release, on every page of every release (site/README.md,
// "Versions"). inject.mjs gives each page `<meta name="docs-version">` and this script; the list of versions is
// versions.json beside it, read at run time, so a release built long ago lists the ones released after it. Without
// the list (offline, an error) the page is as its release built it.

const meta = document.querySelector('meta[name="docs-version"]')
const root = new URL('./', import.meta.url).pathname // '/docx-to-pdf/'

/** The version's base path, and the page's path inside it: '/docx-to-pdf/1.0/' and 'installation/'. */
function where(current) {
  const own = `${root}${current}/`
  const base = location.pathname.startsWith(own) ? own : root
  return { base, rest: location.pathname.slice(base.length) }
}

/** The same page in another version, or that version's home when it has no such page. */
async function go(version, rest) {
  const target = version.path + rest
  try {
    const answer = await fetch(target, { method: 'HEAD' })
    location.href = answer.ok ? target + location.hash : version.path
  } catch {
    location.href = version.path
  }
}

/** The picker's and the banner's words in the page's language (site/locales.mjs), English when it has none. */
const WORDS = {
  en: {
    picker: 'Documentation version',
    latest: (v) => `${v} (latest)`,
    banner: (name, current, latest) => `This is the documentation of ${name} ${current}. The latest release is ${latest}: `,
    same: 'this page in the latest release',
    and: ', and ',
    upgrade: 'how to upgrade',
    changed: 'what changed',
  },
  ru: {
    picker: 'Версия документации',
    latest: (v) => `${v} (последняя)`,
    banner: (name, current, latest) => `Это документация ${name} ${current}. Последний релиз — ${latest}: `,
    same: 'эта страница в последнем релизе',
    and: ' и ',
    upgrade: 'как обновиться',
    changed: 'что изменилось',
  },
  zh: {
    picker: '文档版本',
    latest: (v) => `${v}（最新）`,
    banner: (name, current, latest) => `这是 ${name} ${current} 的文档。最新版本是 ${latest}：`,
    same: '最新版本中的本页',
    and: '，以及',
    upgrade: '如何升级',
    changed: '更新内容',
  },
  es: {
    picker: 'Versión de la documentación',
    latest: (v) => `${v} (última)`,
    banner: (name, current, latest) => `Esta es la documentación de ${name} ${current}. La última versión es ${latest}: `,
    same: 'esta página en la última versión',
    and: ' y ',
    upgrade: 'cómo actualizar',
    changed: 'qué ha cambiado',
  },
}
const words = WORDS[document.documentElement.lang.split('-')[0]] ?? WORDS.en

const css = `
.dv-picker { display: flex; align-items: center; gap: 0.125rem; color: var(--sl-color-gray-1); }
.dv-picker:hover { color: var(--sl-color-white); }
.dv-picker::after { content: '▾'; font-size: var(--sl-text-xs); pointer-events: none; }
.dv-picker select { appearance: none; background: transparent; border: 0; color: inherit;
  font: inherit; font-size: var(--sl-text-sm); padding: 0.25rem 0 0.25rem 0.5rem; cursor: pointer; }
.dv-picker option { background: var(--sl-color-bg); color: var(--sl-color-text); }
.dv-banner { background: var(--sl-color-orange-low); color: var(--sl-color-orange-high);
  border-bottom: 1px solid var(--sl-color-orange); padding: 0.75rem 1rem; text-align: center;
  font-size: var(--sl-text-sm); }
.dv-banner a { color: inherit; font-weight: 600; }
`

function picker(versions, latest, current, rest) {
  const label = document.createElement('label')
  label.className = 'dv-picker'
  const select = document.createElement('select')
  select.setAttribute('aria-label', words.picker)
  for (const v of versions) {
    const option = document.createElement('option')
    option.value = v.version
    option.textContent = v.version === latest ? words.latest(v.version) : v.version
    option.selected = v.version === current
    select.append(option)
  }
  select.addEventListener('change', () => go(versions.find((v) => v.version === select.value), rest))
  label.append(select)
  return label
}

function render({ latest, changelog, versions }) {
  const current = meta.content
  const { rest } = where(current)
  const style = document.createElement('style')
  style.textContent = css
  document.head.append(style)

  // The picker, beside each theme select: the header's, and the mobile menu's.
  for (const theme of document.querySelectorAll('starlight-theme-select')) theme.before(picker(versions, latest, current, rest))

  // The banner of an older release: the same page in the latest, and how to upgrade.
  if (current === latest) return
  const newest = versions.find((v) => v.version === latest)
  const name = document.querySelector('meta[property="og:site_name"]')?.content ?? ''
  const banner = document.createElement('div')
  banner.className = 'dv-banner'
  banner.setAttribute('role', 'note')
  banner.append(words.banner(name, current, latest))
  const same = document.createElement('a')
  same.href = newest.path + rest
  same.textContent = words.same
  same.addEventListener('click', (e) => {
    e.preventDefault()
    go(newest, rest)
  })
  const upgrade = document.createElement('a')
  // The upgrade guide in the page's language: a language's directory is its tag's first part (site/locales.mjs).
  const dir = document.documentElement.lang.split('-')[0]
  const language = rest.startsWith(`${dir}/`) ? `${dir}/` : ''
  upgrade.href = newest.upgrading ? `${newest.path}${language}upgrading/` : changelog
  upgrade.textContent = newest.upgrading ? words.upgrade : words.changed
  banner.append(same, words.and, upgrade, document.documentElement.lang.startsWith('zh') ? '。' : '.')
  // At the head of the page's own column, beside the table of contents rather than under it.
  const column = document.querySelector('main') ?? document.body
  column.prepend(banner)
}

if (meta) {
  fetch(`${root}versions.json`)
    .then((answer) => (answer.ok ? answer.json() : Promise.reject(answer.status)))
    .then(render)
    .catch(() => {})
}
