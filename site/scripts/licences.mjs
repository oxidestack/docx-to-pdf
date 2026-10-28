// Hold every package of package-lock.json to the project's licence allowlist.
//
// The allowlist is deny.toml's `[licenses] allow`, the CNCF's allowed third-party licences, read from there so the
// Rust graph and this one cannot drift apart. A package passes when its SPDX expression is satisfied by allowed
// licences (`A OR B` needs one of them, `A AND B` both).
//
// A package that does not pass needs an entry in BUILD_ONLY: the licence it was reviewed under and why it needs no
// exception. Every entry there is a build tool that runs in `npm run build` and never reaches site/dist/, the only
// thing the site publishes: the pages, Starlight's and Pagefind's scripts and styles (MIT), Expressive Code's (MIT).
// Under the CNCF's licence policy such internal tooling (used only to build, in no built artifact, never fetched onto
// a user's system) needs no exception. A package whose licence changes from the reviewed one fails again, and so does
// an entry that matches no package, so the list never outlives what it describes.
//
//   node scripts/licences.mjs      (site/build.sh runs it before `npm ci`; exit 1 on a finding)

import { readFileSync } from 'node:fs'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'

const here = dirname(fileURLToPath(import.meta.url))
const SITE = join(here, '..')
const REPO = join(SITE, '..')

/** Packages outside the allowlist, by name pattern: the licence reviewed, and why no exception is needed. */
const BUILD_ONLY = [
  {
    name: /^@img\/sharp-(libvips-.+|wasm32|win32-.+)$/,
    licence: /LGPL-3\.0-or-later/,
    why: "libvips, Astro's optional image processor (sharp): it reads images at build time; the site ships no code of it",
  },
  {
    name: /^lightningcss(-.+)?$/,
    licence: /^MPL-2\.0$/,
    why: "Vite's CSS transformer: the CSS it writes is Starlight's (MIT), none of its own",
  },
  {
    name: /^(lru-cache|sax|common-ancestor-path)$/,
    licence: /^BlueOak-1\.0\.0$/,
    why: 'a cache, an XML parser and a path helper of the build (Astro, svgo, the sitemap); none is bundled',
  },
  {
    name: /^mdn-data$/,
    licence: /^CC0-1\.0$/,
    why: "CSS syntax data read by css-tree and csso while the build minifies; the site's CSS carries none of it",
  },
]

function allowlist() {
  const toml = readFileSync(join(REPO, 'deny.toml'), 'utf8')
  const section = toml.split(/^\[licenses\]$/m)[1]
  const list = section?.match(/^allow = \[([^\]]*)\]/m)?.[1]
  if (!list) throw new Error('deny.toml: no `allow = [...]` under [licenses]')
  return new Set([...list.matchAll(/"([^"]+)"/g)].map((m) => m[1]))
}

/** Whether the SPDX expression is satisfied by the allowed licences: OR binds looser than AND, parentheses group. */
function satisfied(expression, allowed) {
  const tokens = expression.match(/\(|\)|[^\s()]+/g) ?? []
  let at = 0
  const or = () => {
    let ok = and()
    while (tokens[at] === 'OR') {
      at++
      ok = and() || ok
    }
    return ok
  }
  const and = () => {
    let ok = atom()
    while (tokens[at] === 'AND') {
      at++
      ok = atom() && ok
    }
    return ok
  }
  const atom = () => {
    const token = tokens[at++]
    if (token === '(') {
      const ok = or()
      at++ // ')'
      return ok
    }
    return allowed.has(token)
  }
  const ok = or()
  return ok && at === tokens.length
}

const allowed = allowlist()
const lock = JSON.parse(readFileSync(join(SITE, 'package-lock.json'), 'utf8'))
const findings = []
const used = new Set()
let count = 0
for (const [path, meta] of Object.entries(lock.packages)) {
  if (path === '') continue
  count++
  const name = path.slice(path.lastIndexOf('node_modules/') + 'node_modules/'.length)
  const licence = meta.license
  if (typeof licence !== 'string' || licence === '') {
    findings.push(`${name}@${meta.version}: no licence in package-lock.json`)
    continue
  }
  if (satisfied(licence, allowed)) continue
  const entry = BUILD_ONLY.find((e) => e.name.test(name))
  if (!entry) {
    findings.push(`${name}@${meta.version}: ${licence} is not on deny.toml's allowlist and not a reviewed build tool`)
  } else if (!entry.licence.test(licence)) {
    findings.push(`${name}@${meta.version}: ${licence}, reviewed as ${entry.licence}: review it again`)
  } else {
    used.add(entry)
  }
}
for (const entry of BUILD_ONLY) {
  if (!used.has(entry)) findings.push(`BUILD_ONLY ${entry.name}: matches no package of package-lock.json, remove it`)
}
for (const finding of findings) console.error(`licences: ${finding}`)
if (findings.length) process.exit(1)
console.log(`licences: ${count} packages, each allowlisted or a reviewed build tool (${used.size} entries)`)
