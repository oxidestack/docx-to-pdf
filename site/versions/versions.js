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
  select.setAttribute('aria-label', 'Documentation version')
  for (const v of versions) {
    const option = document.createElement('option')
    option.value = v.version
    option.textContent = v.version === latest ? `${v.version} (latest)` : v.version
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
  banner.append(`This is the documentation of ${name} ${current}. The latest release is ${latest}: `)
  const same = document.createElement('a')
  same.href = newest.path + rest
  same.textContent = 'this page in the latest release'
  same.addEventListener('click', (e) => {
    e.preventDefault()
    go(newest, rest)
  })
  const upgrade = document.createElement('a')
  upgrade.href = newest.upgrading ? `${newest.path}upgrading/` : changelog
  upgrade.textContent = newest.upgrading ? 'how to upgrade' : 'what changed'
  banner.append(same, ', and ', upgrade, '.')
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
