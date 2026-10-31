// The last step of `make site-versions` (site/README.md, "Versions"): the releases' sites, each built by its own tag,
// become one site.
//
//   node inject.mjs <site> <root path> <repository URL> <version>...   (the versions newest first: 1.1 1.0)
//
// <site> holds the latest release at its root and every release under <x.y>/. This writes <site>/versions.json, the
// list the picker reads at run time, so a release built long ago lists the ones after it; copies versions.js beside
// it; and gives every page of every release the same three lines in its head: which version it is, the script that
// draws the picker and the banner, and, under <x.y>/, `noindex`, so a search engine finds the root's copy of a page.
// Nothing of a release's own build changes: a release made before the picker existed gets it the same way.

import { readFileSync, writeFileSync, readdirSync, statSync, existsSync, copyFileSync } from 'node:fs'
import { join, dirname } from 'node:path'
import { fileURLToPath } from 'node:url'

const here = dirname(fileURLToPath(import.meta.url))

/** Every .html file under `dir`, but not under the directories `skip` names. */
function pages(dir, skip = new Set()) {
  const out = []
  for (const name of readdirSync(dir)) {
    const path = join(dir, name)
    if (statSync(path).isDirectory()) {
      if (!skip.has(name)) out.push(...pages(path))
    } else if (name.endsWith('.html')) out.push(path)
  }
  return out
}

/** The page with the version's lines at the end of its head; a page that has them already is left as it is. */
export function inject(html, { version, root, archived }) {
  if (html.includes('<meta name="docs-version"')) return html
  const lines = [
    `<meta name="docs-version" content="${version}">`,
    archived ? '<meta name="robots" content="noindex">' : '',
    `<script type="module" src="${root}/versions.js"></script>`,
  ].join('')
  const at = html.indexOf('</head>')
  if (at === -1) return html
  return html.slice(0, at) + lines + html.slice(at)
}

/** versions.json: the latest first, each version with where it is read and whether it has an upgrade guide. */
export function manifest(site, root, repository, versions) {
  const [latest] = versions
  return {
    latest,
    changelog: `${repository}/blob/main/CHANGELOG.md`,
    versions: versions.map((version) => ({
      version,
      path: version === latest ? `${root}/` : `${root}/${version}/`,
      upgrading: existsSync(join(site, version, 'upgrading', 'index.html')),
    })),
  }
}

export function main([site, root, repository, ...versions]) {
  if (!site || !root || !repository || versions.length === 0) {
    throw new Error('usage: node inject.mjs <site> <root path> <repository URL> <version>... (newest first)')
  }
  const [latest] = versions
  const write = (file, version, archived) =>
    writeFileSync(file, inject(readFileSync(file, 'utf8'), { version, root, archived }))
  for (const file of pages(site, new Set(versions))) write(file, latest, false)
  // Every <x.y>/ is noindex, the latest's too: its pages are the root's, which is the copy to find.
  for (const version of versions) for (const file of pages(join(site, version))) write(file, version, true)
  writeFileSync(join(site, 'versions.json'), `${JSON.stringify(manifest(site, root, repository, versions), null, 2)}\n`)
  copyFileSync(join(here, 'versions.js'), join(site, 'versions.js'))
}

if (process.argv[1] === fileURLToPath(import.meta.url)) main(process.argv.slice(2))
