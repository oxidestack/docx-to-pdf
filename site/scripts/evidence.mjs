// The evidence: the Benchmarks page, the README's headline numbers and the comparison guide's figures, all generated
// from one committed file, the raw results of the latest full `make compare` run (public/benchmarks/results.json,
// served beside the page for download).
//
// A number a reader is shown has to be one a stranger can find in that file, so nothing here types a figure by hand:
//
//   * Every value read from the results goes through `v()`, which marks it. The check strips the marked spans, the
//     markup and the link targets from the generated text, and fails on any digit left over, so a figure typed into
//     a template below fails the build. The few labels that hold a digit and are not figures (p50, 2σ, SHA-256, amd64, QUICK=1) are
//     listed in LABELS.
//   * The README and docs/guide/comparison.md each carry a block between two markers, exactly as `headline()` and
//     `guide()` write it: a block that differs from the generated one, a number edited in it included, fails the
//     check. `--write-readme` rewrites both.
//   * Outside its block, a number with a unit of measure (×, %, s, MiB, GiB, MB, conversions) fails the check: each
//     file states figures only through its block (a setting is written as code, `180s`, and is not a figure).
//
//   node scripts/evidence.mjs                 the page into src/content/docs/benchmarks.md, after the files' check
//   node scripts/evidence.mjs --write-readme  rewrite both files' blocks from the results (`make evidence`)
//
// package.json's prebuild runs it after scripts/stage.mjs, which empties src/content/docs/; exit 1 on a finding.

import { readFileSync, writeFileSync, mkdirSync, existsSync } from 'node:fs'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'

const here = dirname(fileURLToPath(import.meta.url))
const REPO = join(here, '..', '..')
export const RESULTS = join(here, '..', 'public', 'benchmarks', 'results.json')
const PAGE = join(here, '..', 'src', 'content', 'docs', 'benchmarks.md')
const README = join(REPO, 'README.md')
const GUIDE = join(REPO, 'docs', 'guide', 'comparison.md')

const SITE = 'https://oxidestack.github.io/docx-to-pdf'
const GITHUB = 'https://github.com/oxidestack/docx-to-pdf'
export const BEGIN = '<!-- benchmarks:begin: `make evidence` writes this block from site/public/benchmarks/results.json -->'
const END = '<!-- benchmarks:end -->'

// ---------------------------------------------------------------------------------------------------------------
// Provenance

const OPEN = '\u0001'
const CLOSE = '\u0002'

/** A value read from the results, marked so the check knows it was not typed here. */
export const v = (x) => `${OPEN}${x}${CLOSE}`

/** Labels with a digit that are names, not figures. */
const LABELS = [/\bp(50|95|99)\b/g, /2σ = 2·√\(σ₁²\/n₁ \+ σ₂²\/n₂\)/g, /2σ/g, /SHA-256/g, /\bamd64\b/g, /\bQUICK=1\b/g]

/** The digits of `text` that no value of the results accounts for, each with its context. */
export function strays(text) {
  let rest = text
    .replace(new RegExp(`${OPEN}[^${CLOSE}]*${CLOSE}`, 'g'), '')
    .replace(/<!--[\s\S]*?-->/g, '')
    .replace(/<[^>]*>/g, ' ')
    .replace(/\]\([^)]*\)/g, ']')
  for (const label of LABELS) rest = rest.replace(label, '')
  const out = []
  for (const m of rest.matchAll(/\d/g)) out.push(rest.slice(Math.max(0, m.index - 30), m.index + 30).replace(/\s+/g, ' '))
  return out
}

/** The text a reader sees: the marks removed. */
export const plain = (text) => text.replaceAll(OPEN, '').replaceAll(CLOSE, '')

/** A number with a unit of measure, the shape of a figure, outside code. */
const FIGURE = /\d[\d.,]*(?:[  ]?(?:×|%|MiB|GiB|KiB|MB|GB|ms|s\b|conversions))/g

/** Figure-shaped numbers in a file (the README, the comparison guide) outside its generated block and outside code. */
export function readmeStrays(readme) {
  const out = []
  const start = readme.indexOf(BEGIN)
  const end = readme.indexOf(END)
  const outside = start === -1 || end === -1 ? readme : readme.slice(0, start) + readme.slice(end + END.length)
  let fenced = false
  for (const line of outside.split('\n')) {
    if (/^\s*(```|~~~)/.test(line)) {
      fenced = !fenced
      continue
    }
    if (fenced) continue
    const prose = line.replace(/`[^`]*`/g, '').replace(/\]\([^)]*\)/g, ']')
    for (const m of prose.matchAll(FIGURE)) out.push(`${m[0]} in: ${line.trim()}`)
  }
  return out
}

// ---------------------------------------------------------------------------------------------------------------
// Figures: tools/bench/figures.py's mean ± σ (n), in JavaScript

export function stats(values) {
  const xs = values.filter((x) => x !== null && x !== undefined && !Number.isNaN(x))
  if (!xs.length) return null
  const mean = xs.reduce((a, b) => a + b, 0) / xs.length
  const sd = xs.length > 1 ? Math.sqrt(xs.reduce((a, x) => a + (x - mean) ** 2, 0) / (xs.length - 1)) : null
  return { mean, sd, n: xs.length }
}

export function fmt(x, digits) {
  if (Math.abs(x) >= 100) return Math.round(x).toLocaleString('en-US').replace(/,/g, ' ')
  return x.toFixed(digits)
}

/** σ with the mean's decimals, and at least one significant digit. */
export function fmtSigma(mean, sd, digits) {
  let places = Math.abs(mean) < 100 ? digits : 0
  while (places < 4 && sd && Number(sd.toFixed(places)) === 0) places += 1
  return sd.toFixed(places)
}

/** `mean ± σ (n)`, every part marked. */
export function record(s, digits) {
  if (!s) return '—'
  if (s.sd === null) return `${v(fmt(s.mean, digits))} (${v(s.n)} run, no σ)`
  return `${v(fmt(s.mean, digits))} ± ${v(fmtSigma(s.mean, s.sd, digits))} (${v(s.n)})`
}

/** The ratio of two means ± its standard error, by propagation. */
export function ratio(a, b) {
  const r = a.mean / b.mean
  const se = r * Math.sqrt((a.sd ** 2 / a.n) / a.mean ** 2 + (b.sd ** 2 / b.n) / b.mean ** 2)
  return { r, se }
}

export const fmtRatio = ({ r, se }) => `${v(r.toFixed(2))} ± ${v(fmtSigma(r, se, 2))}×`

/** Which of two figures is ahead at 2σ: 1 the first, -1 the second, 0 neither. */
export function ahead(a, b, higherIsBetter) {
  const bound = 2 * Math.sqrt(a.sd ** 2 / a.n + b.sd ** 2 / b.n)
  const d = higherIsBetter ? a.mean - b.mean : b.mean - a.mean
  if (Math.abs(a.mean - b.mean) <= bound) return 0
  return d > 0 ? 1 : -1
}

// The speed cell's figures, as compare.py's ROWS: (id, label, read, decimals, higher is better).
const MIB = 2 ** 20
export const ROWS = [
  ['throughput', 'conversions/s', (s) => s.throughput, 3, true],
  ['cpu', 'CPU-s a conversion', (s) => s.cpu_seconds_per_conversion, 3, false],
  ['p50', 'p50 latency, s', (s) => s.latency?.p50, 2, false],
  ['p95', 'p95 latency, s', (s) => s.latency?.p95, 2, false],
  ['p99', 'p99 latency, s', (s) => s.latency?.p99, 2, false],
  ['peak', 'container `memory.peak`, MiB', (s) => s.memory_peak / MIB, 0, false],
  ['anon', 'anonymous memory peak, MiB', (s) => s.anon_max / MIB, 0, false],
  ['written', 'written a conversion, MB', (s) => s.write_bytes_per_conversion / 1e6, 1, false],
  ['ready', '`docker run` to ready, s', (s) => s.ready_seconds, 2, false],
]

// The services, by repository; the speed cell runs Gotenberg twice, as it ships and without its restarts.
export const SPEED = [
  ['ours', 'oxidestack/docx-to-pdf'],
  ['gotenberg', 'Gotenberg, as shipped'],
  ['g-norestart', 'Gotenberg, no restarts'],
  ['oracle', 'moalhaddar/docx-to-pdf'],
]
const TORTURE = [
  ['ours', 'oxidestack/docx-to-pdf'],
  ['gotenberg', 'Gotenberg'],
  ['moalhaddar', 'moalhaddar/docx-to-pdf'],
]
const SOAK = [
  ['ours', 'oxidestack/docx-to-pdf'],
  ['gotenberg', 'Gotenberg, as shipped'],
  ['g-norestart', 'Gotenberg, no restarts'],
  ['moalhaddar', 'moalhaddar/docx-to-pdf'],
]

/** Every figure of the speed cell: figures[subject][row id] = {mean, sd, n}. */
export function speedFigures(results) {
  const out = {}
  for (const [subject] of SPEED) {
    const runs = results.speed.cells[subject] ?? []
    out[subject] = Object.fromEntries(ROWS.map(([id, , read]) => [id, stats(runs.map(read))]))
  }
  return out
}

/** What a torture cell says, read from the cell compare.py wrote from the raw answers. */
export function parseCell(cell) {
  const m = /^(no answer|\d+) in ([\d.]+) s; peak ([\d ]+) MiB/.exec(cell)
  if (!m) return null
  const pictures = /pictures in the PDF (\d+)/.exec(cell)
  return {
    status: m[1],
    seconds: Number(m[2]),
    peak: Number(m[3].replace(/ /g, '')),
    pictures: pictures ? Number(pictures[1]) : null,
  }
}

/** A torture cell as the table shows it: the answer, its time and peak, the pictures; never the body's excerpt. */
function shortCell(cell) {
  return cell.split('; “')[0].replace(/\|/g, '\\|')
}

/** The second half's LibreOffice level of a soak cell, `{mean, sd, n}`. */
export function soakLevel(cell) {
  const m = /([\d ]+) ± ([\d.]+) MiB \((\d+) requests\) over the second half/.exec(cell)
  return m ? { mean: Number(m[1].replace(/ /g, '')), sd: Number(m[2]), n: Number(m[3]) } : null
}

// ---------------------------------------------------------------------------------------------------------------
// What the run was

const at = (results) => {
  const s = results.meta.stamp
  return `${v(`${s.slice(0, 4)}-${s.slice(4, 6)}-${s.slice(6, 8)} ${s.slice(9, 11)}:${s.slice(11, 13)}`)} UTC`
}

function host(results) {
  const h = results.meta.host
  const caches = Object.entries(h.caches ?? {})
    .map(([k, x]) => `${v(k)} ${v(x)}`)
    .join(', ')
  return [
    `${v(h.cpu_model)} (${v(h.arch)}), ${v(h.vcpus)} vCPUs seen as ${v(h.sockets)} socket × ${v(h.cores_per_socket)} cores,`,
    `${v(h.threads_per_core)} thread a core, ${v(Math.round(h.mhz_now))} MHz reported; caches ${caches};`,
    `${v((h.memory_bytes / 2 ** 30).toFixed(1))} GiB; ${v(h.os)}, kernel ${v(h.kernel)}, Docker ${v(h.docker)},`,
    `cgroup v${v(h.cgroup)}; a virtual machine under ${v(h.hypervisor)}, so the vCPUs' pinning to the host's cores is not`,
    'visible from the guest',
  ].join(' ')
}

function cellSetup(results) {
  const r = results.speed.cells.ours[0]
  return {
    cpus: v(r.cpus),
    pool: v(r.pool),
    memory: v(r.memory.replace(/g$/, ' GiB')),
    docs: r.docs.split(',').map((d) => `\`${v(d)}\``).join(', '),
    conversions: v(results.meta.conversions),
    blocks: v(results.meta.blocks),
    seed: v(results.speed.seed ?? results.meta.seed),
  }
}

// ---------------------------------------------------------------------------------------------------------------
// The headline block (the README, and so the site's home page)

// Which code the run measured. Its commit is in results.json (meta.commit) but not in the published history, so the
// pages name the day and the change since instead; say what that change costs once a run has measured it.
const CODE_NOTE = 'a release image carries it, at a small CPU cost a conversion that this run did not measure'

/**
 * The release a run measured: its version when the image is a published release pulled by digest
 * (`<registry>/oxidestack/docx-to-pdf:<version>@sha256:<digest>`, `make compare IMAGE=`), or null when the command
 * built the image from a checkout. The texts below say which code was measured from it.
 */
export function release(meta) {
  const m = /^[^@\s]*\/oxidestack\/docx-to-pdf:(\d+\.\d+\.\d+)@sha256:[0-9a-f]{64}$/.exec(meta.image ?? '')
  return m ? m[1] : null
}

/** The README block's sentence on the code measured. */
const measuredReadme = (meta) =>
  release(meta)
    ? `The image measured is release ${v(release(meta))}, pulled by digest.`
    : `The code measured is that day's, before the heap trim, the change that hands LibreOffice's free heap back after each conversion: ${CODE_NOTE}.`

/** The comparison guide's sentence on the code measured. */
const measuredGuide = (meta) =>
  release(meta)
    ? `oxidestack/docx-to-pdf was release ${v(release(meta))}, its published image pulled by digest.`
    : `oxidestack/docx-to-pdf was the code of that day, before the heap trim, the change that hands LibreOffice's free heap back after each conversion: ${CODE_NOTE}.`

// The one sentence of the project's positioning: what the README opens with. Each clause stands on a figure the block
// and the Benchmarks page show; it names the two services measured, never "anything else".
const SENTENCE =
  'docx-to-pdf turns office documents into PDF faster and with less memory than Gotenberg and moalhaddar/docx-to-pdf on the same documents, and no document — however large, malicious or broken — can take it down, hold it past its timeout or make it reach the network.'

// The deflate bomb of the torture test, the headline's fourth row.
const BOMB = 'pages-500000p'

const row = (id) => `${SITE}/benchmarks/#${id}`

export function headline(results) {
  const f = speedFigures(results)
  const r = (a, b, k) => fmtRatio(ratio(f[a][k], f[b][k]))
  const deflate = Object.fromEntries(
    results.torture.filter((t) => t.case === BOMB).map((t) => [t.subject, parseCell(t.cell)]),
  )
  if (!deflate.ours) throw new Error(`the results have no torture case ${BOMB}`)
  const mib = (x) => v(x.toLocaleString('en-US').replace(/,/g, ' '))
  const answer = (c) => (c.status === 'no answer' ? `no answer in ${v(c.seconds)} s` : `${v(c.status)} in ${v(c.seconds)} s`)
  const setup = cellSetup(results)
  const h = results.meta.host
  // One logical line a paragraph or item; readmeBlock() wraps them to the repository's width.
  return [
    BEGIN,
    '',
    `> **${SENTENCE}**`,
    '',
    `- **Less CPU than Gotenberg**: Gotenberg uses ${r('gotenberg', 'ours', 'cpu')} the CPU a conversion as it ships, and ${r('g-norestart', 'ours', 'cpu')} without its restarts${release(results.meta) ? '' : ', measured on the code before the heap trim'} ([the row](${row('ratio-cpu')})).`,
    `- **Less memory than Gotenberg**: its container peaks at ${r('gotenberg', 'ours', 'peak')} the memory as it ships, and ${r('g-norestart', 'ours', 'peak')} without its restarts ([the row](${row('ratio-peak')})).`,
    `- **The service it replaces, outrun**: ${r('ours', 'oracle', 'throughput')} moalhaddar/docx-to-pdf's throughput, which uses ${r('oracle', 'ours', 'cpu')} the CPU a conversion and ${r('oracle', 'ours', 'peak')} the memory ([the row](${row('ratio-throughput')})).`,
    `- **A deflate bomb stopped at once**: \`${v(BOMB)}\`, a document that inflates past any limit, gets ${answer(deflate.ours)} at a ${mib(deflate.ours.peak)} MiB peak; Gotenberg answers ${answer(deflate.gotenberg)} while its LibreOffice goes on to ${mib(deflate.gotenberg.peak)} MiB, and moalhaddar/docx-to-pdf gives ${answer(deflate.moalhaddar)} at ${mib(deflate.moalhaddar.peak)} MiB, one run a side ([the row](${row(`torture-${BOMB}`)})).`,
    '',
    `Measured by \`make compare\` on ${at(results)}, on an ${v(h.cpu_model)} (${v(h.arch)}), ${setup.cpus} pinned vCPU and a pool of ${setup.pool} a service: mean ± σ (n) over ${setup.blocks} interleaved blocks, a ratio ± its standard error. ${measuredReadme(results.meta)} [Benchmarks](${SITE}/benchmarks/) has every figure, the rows where Gotenberg wins, and the raw results.`,
    '',
    END,
  ].join('\n')
}

const WIDTH = 120

/**
 * Markdown wrapped at WIDTH, as the repository's prose is: a quote's lines keep `> `, an item's go on indented. A
 * number's thousands (`7 610`) are never split.
 */
export function wrap(text) {
  const out = []
  for (const line of text.split('\n')) {
    if (line.length <= WIDTH || line.startsWith('<!--') || line.startsWith('|')) {
      out.push(line)
      continue
    }
    const [lead, next] = line.startsWith('> ') ? ['> ', '> '] : line.startsWith('- ') ? ['- ', '  '] : ['', '']
    // A link's text is never split from its target: its spaces are held while the line is cut.
    const held = line.slice(lead.length).replace(/\[[^\]]*\]\(/g, (m) => m.replaceAll(' ', '\u0003'))
    const words = held.split(' ')
    const tokens = []
    for (const w of words) {
      const last = tokens.length - 1
      if (last >= 0 && /^\d{3}\b/.test(w) && /(^|[^\d.])\d{1,3}$/.test(tokens[last])) tokens[last] += ` ${w}`
      else tokens.push(w)
    }
    let cur = lead
    let first = true
    for (const t of tokens) {
      const prefix = first ? lead : next
      if (cur.length > prefix.length && cur.length + 1 + t.length > WIDTH) {
        out.push(cur)
        cur = next + t
        first = false
      } else cur = cur.length > prefix.length ? `${cur} ${t}` : cur + t
    }
    out.push(cur)
  }
  return out.join('\n').replaceAll('\u0003', ' ')
}

/** The README's block as it must read: the headline, unmarked and wrapped. */
export const readmeBlock = (results) => wrap(plain(headline(results)))

/** The comparison guide's block as it must read. */
export const guideBlock = (results) => wrap(plain(guide(results)))

// ---------------------------------------------------------------------------------------------------------------
// The Benchmarks page

const SERIES = ['var(--sl-color-accent)', 'var(--sl-color-gray-3)', 'var(--sl-color-gray-4)', 'var(--sl-color-gray-5)']

/** A horizontal bar chart of one figure, one bar a service, a whisker of ±σ; every label a marked value. */
function chart(id, label, f, digits) {
  const W = 640
  const left = 200
  const bar = 22
  const gap = 10
  const top = 28
  const max = Math.max(...SPEED.map(([s]) => f[s][id].mean + (f[s][id].sd ?? 0)))
  const scale = (x) => ((W - left - 110) * x) / max
  const H = top + SPEED.length * (bar + gap)
  const parts = [
    `<svg role="img" aria-label="${label}" viewBox="0 0 ${W} ${H}" width="100%" style="max-width:${W}px;font:13px var(--sl-font, sans-serif)">`,
    `<text x="0" y="16" fill="var(--sl-color-white)" font-weight="600">${label}</text>`,
  ]
  SPEED.forEach(([s, name], i) => {
    const y = top + i * (bar + gap)
    const { mean, sd } = f[s][id]
    const w = scale(mean)
    parts.push(
      `<text x="${left - 8}" y="${y + bar / 2 + 4}" text-anchor="end" fill="var(--sl-color-gray-2)">${name}</text>`,
      `<rect x="${left}" y="${y}" width="${w.toFixed(1)}" height="${bar}" rx="3" fill="${SERIES[i]}"/>`,
    )
    if (sd) {
      const x0 = left + scale(mean - sd)
      const x1 = left + scale(mean + sd)
      parts.push(
        `<line x1="${x0.toFixed(1)}" x2="${x1.toFixed(1)}" y1="${y + bar / 2}" y2="${y + bar / 2}" stroke="var(--sl-color-white)" stroke-width="1.5"/>`,
      )
    }
    parts.push(
      `<text x="${(left + scale(mean + (sd ?? 0)) + 8).toFixed(1)}" y="${y + bar / 2 + 4}" fill="var(--sl-color-gray-2)">${v(fmt(mean, digits))}${sd ? ` ± ${v(fmtSigma(mean, sd, digits))}` : ''}</text>`,
    )
  })
  parts.push('</svg>')
  return parts.join('')
}

const VERDICT = { 1: 'oxidestack/docx-to-pdf ahead', 0: 'no difference at 2σ', '-1': 'Gotenberg ahead' }

/** The rows where Gotenberg is ahead of oxidestack/docx-to-pdf, and those where neither is, from every table. */
export function gotenbergRows(results, base = '') {
  const f = speedFigures(results)
  const wins = []
  const ties = []
  for (const [g, gname] of SPEED.slice(1, 3)) {
    for (const [id, label, , digits, higher] of ROWS) {
      const a = f.ours[id]
      const b = f[g][id]
      if (!a?.sd || !b?.sd) continue
      const verdict = ahead(a, b, higher)
      const line = `${gname}, ${label}: ${record(b, digits)} against ${record(a, digits)}`
      if (verdict === -1) wins.push(`${line} ([the row](${base}#speed-${id}))`)
      if (verdict === 0) ties.push(`${line} ([the row](${base}#speed-${id}))`)
    }
  }
  const cases = [...new Set(results.torture.filter((t) => t.kind === 'one').map((t) => t.case))]
  for (const c of cases) {
    const ours = parseCell(results.torture.find((t) => t.case === c && t.subject === 'ours').cell)
    const got = parseCell(results.torture.find((t) => t.case === c && t.subject === 'gotenberg').cell)
    if (!ours || !got || ours.status !== got.status || ours.pictures !== got.pictures) continue
    const what = []
    if (got.seconds < ours.seconds) what.push(`answered in ${v(got.seconds)} s against ${v(ours.seconds)} s`)
    if (got.peak < ours.peak) what.push(`peaked at ${v(got.peak)} MiB against ${v(ours.peak)} MiB`)
    if (what.length)
      wins.push(`Gotenberg, \`${v(c)}\` (the same answer, ${v(got.status)}, and the same pictures): ${what.join(', ')}, one run a side ([the row](${base}#torture-${c}))`)
  }
  const level = (l) => `${v(fmt(l.mean, 0))} ± ${v(fmtSigma(l.mean, l.sd, 0))} MiB over ${v(l.n)} conversions`
  const soak = Object.fromEntries(
    results.torture.filter((t) => t.kind === 'soak').map((t) => [t.subject, soakLevel(t.cell)]),
  )
  for (const [g, gname] of SOAK.slice(1, 3)) {
    if (!soak.ours || !soak[g]) continue
    if (ahead(soak.ours, soak[g], false) === -1)
      wins.push(
        `${gname}, LibreOffice's resident memory over the soak's second half: ${level(soak[g])} against ${level(soak.ours)}, one soak a side ([the row](${base}#soaks))`,
      )
  }
  return { wins, ties }
}

// The four figures the ratio tables show.
const RATIOS = ['throughput', 'cpu', 'p99', 'peak']

/** Each service's figure over oxidestack/docx-to-pdf's, a row a figure; `head(id, label)` writes a row's first cell. */
function ratioTable(f, head) {
  const L = [`| | ${SPEED.slice(1).map(([, n]) => n).join(' | ')} |`, `| --- |${SPEED.slice(1).map(() => ' --- |').join('')}`]
  for (const [id, label, , , higher] of ROWS.filter(([id]) => RATIOS.includes(id))) {
    const cells = SPEED.slice(1).map(([s]) => {
      const verdict = ahead(f.ours[id], f[s][id], higher)
      return `${fmtRatio(ratio(f[s][id], f.ours[id]))} (${VERDICT[verdict].replace('Gotenberg', s === 'oracle' ? 'moalhaddar/docx-to-pdf' : 'Gotenberg')})`
    })
    L.push(`| ${head(id, label)} | ${cells.join(' | ')} |`)
  }
  return L
}

/** The torture cases, a row a case (the soaks apart); `head(case)` writes a row's first cell. */
function tortureTable(results, head) {
  const L = [`| Case | ${TORTURE.map(([, n]) => n).join(' | ')} |`, `| --- |${TORTURE.map(() => ' --- |').join('')}`]
  const cases = [...new Set(results.torture.filter((t) => t.kind !== 'soak').map((t) => t.case))]
  for (const c of cases) {
    const cells = TORTURE.map(([s]) => {
      const t = results.torture.find((x) => x.case === c && x.subject === s)
      return t ? v(shortCell(t.cell)) : '—'
    })
    L.push(`| ${head(c)} | ${cells.join(' | ')} |`)
  }
  return L
}

export function page(results) {
  const f = speedFigures(results)
  const setup = cellSetup(results)
  const meta = results.meta
  const L = []
  L.push('---', 'title: Benchmarks', 'description: "Every figure of the latest full make compare run: oxidestack/docx-to-pdf, Gotenberg and moalhaddar/docx-to-pdf, with the raw results."', '---', '')
  L.push(
    `Every number on this page is read from one file, the [raw results](/docx-to-pdf/benchmarks/results.json) of the`,
    `latest full run of \`make compare\`, the comparison anyone can rerun ([how](#run-it-yourself)). The site's build`,
    'generates this page, the README\'s headline numbers and the home page from it, and fails on a number that is not',
    'read from it. Each figure is the mean ± σ (n) over the blocks of the run, σ the sample standard deviation; two',
    'figures differ when their gap is more than 2σ = 2·√(σ₁²/n₁ + σ₂²/n₂).',
    '',
    '## The run',
    '',
    '| | |',
    '| --- | --- |',
    `| When | ${at(results)}, ${v(Math.round(meta.elapsed_s / 60))} minutes |`,
    `| Command | \`make compare\` (\`${v(meta.command.trim())}\`), the full mode |`,
    release(meta)
      ? `| Code | release ${v(release(meta))}, its published image \`${v(meta.image)}\`, built from commit \`${v(meta.image_revision)}\` |`
      : `| Code | the code of that day, built by the command into \`${v(meta.images['oxidestack/docx-to-pdf'])}\`. It predates the change that hands LibreOffice's free heap back to the system after each conversion, so it is not a release image; ${CODE_NOTE} |`,
    `| Host | ${host(results)} |`,
    `| The speed cell | ${setup.cpus} pinned vCPU, a pool of ${setup.pool}, ${setup.memory} and \`/tmp\` on the host's disk a service; a closed load of the typical reports ${setup.docs}; ${setup.conversions} conversions a cell after a warm-up; ${setup.blocks} blocks, each running every cell once in a shuffled order (seed ${setup.seed}) |`,
    `| The torture cases | each case at ${v(meta.torture_memory.replace(/g$/, ' GiB'))} on vCPUs ${v(meta.torture_cpus)}, each case waiting up to ${v(meta.cap)} s; soaks of ${v(meta.soak)} conversions (${v(meta.soak_moalhaddar)} for moalhaddar/docx-to-pdf) alongside |`,
    '',
    '## Speed and memory',
    '',
    `| | ${SPEED.map(([, n]) => n).join(' | ')} |`,
    `| --- |${SPEED.map(() => ' --- |').join('')}`,
  )
  for (const [id, label, , digits] of ROWS) {
    L.push(`| <a id="speed-${id}"></a>${label} | ${SPEED.map(([s]) => record(f[s][id], digits)).join(' | ')} |`)
  }
  L.push('')
  for (const [id, label, , digits] of ROWS.filter(([id]) => ['throughput', 'cpu', 'p99', 'peak'].includes(id))) {
    L.push(chart(id, label.replace(/`/g, ''), f, digits), '')
  }
  L.push(
    '## Against oxidestack/docx-to-pdf',
    '',
    'Each service\'s figure over oxidestack/docx-to-pdf\'s, the ratio of the means ± its standard error, from the same',
    'interleaved run. A ratio measured inside one run holds from one host to the next better than an absolute figure.',
    '',
    ...ratioTable(f, (id, label) => `<a id="ratio-${id}"></a>${label}`),
  )
  const { wins, ties } = gotenbergRows(results)
  L.push(
    '',
    '## Where Gotenberg wins',
    '',
    'Gotenberg is the generalist, and on its own ground nothing here competes with it: HTML and URLs to PDF, merging and',
    'splitting PDFs, PDF/A and PDF/UA, webhooks, clients in many languages and years in production. A team that needs',
    'those should use it. In this run, the rows where it is ahead of oxidestack/docx-to-pdf:',
    '',
    ...(wins.length ? wins.map((w) => `- ${w}`) : ['- none in this run']),
    '',
    'And the rows where neither is ahead at 2σ:',
    '',
    ...(ties.length ? ties.map((t) => `- ${t}`) : ['- none in this run']),
    '',
    '## The torture test',
    '',
    'Hostile and borderline documents, one run a side, each service as it ships: the answer, its time, the container\'s',
    'memory peak and, for a picture, how many pictures reached the PDF. Judged by the answer: the times and peaks have no σ.',
    '',
    ...tortureTable(results, (c) => `<a id="torture-${c}"></a>\`${v(c)}\``),
  )
  L.push(
    '',
    '## Soaks',
    '',
    '<a id="soaks"></a>LibreOffice\'s resident memory after each conversion of the typical reports, one soak a side:',
    'the first tenth, the second half, and the least-squares slope.',
    '',
    '| Service | Soak |',
    '| --- | --- |',
  )
  for (const [s, name] of SOAK) {
    const t = results.torture.find((x) => x.kind === 'soak' && x.subject === s)
    if (t) L.push(`| ${name} | ${v(t.cell.replace(/\|/g, '\\|'))} |`)
  }
  L.push('', '## Versions', '', '| | Image |', '| --- | --- |')
  for (const [name, ref] of Object.entries(meta.images)) L.push(`| ${v(name)} | \`${v(ref)}\` |`)
  L.push('', '## Documents', '', 'Generated with fixed seeds, or fetched by commit; each one\'s SHA-256:', '', '| Document | SHA-256 |', '| --- | --- |')
  for (const [name, sha] of Object.entries(meta.documents)) L.push(`| \`${v(name)}\` | \`${v(sha)}\` |`)
  L.push(
    '',
    '## Run it yourself',
    '',
    'Docker is all it needs; a quick mode prints both tables in minutes on a laptop, and moalhaddar/docx-to-pdf, built',
    'for amd64 only, is skipped on another architecture:',
    '',
    '```sh',
    'git clone --recursive https://github.com/oxidestack/docx-to-pdf.git && cd docx-to-pdf',
    'make compare QUICK=1   # minutes on a laptop',
    'make compare           # the full mode, as above',
    '```',
    '',
    `[\`examples/compare/\`](${GITHUB}/tree/main/examples/compare) says what each mode runs. The run writes the tables`,
    'and a `results.json` like [this one](/docx-to-pdf/benchmarks/results.json).',
  )
  return `${L.join('\n')}\n`
}

// ---------------------------------------------------------------------------------------------------------------

// ---------------------------------------------------------------------------------------------------------------
// The comparison guide's block (docs/guide/comparison.md): its two measured sections, the ratios and the torture
// cases, each row linking its figures on the Benchmarks page.

// What each torture case is, in words; a case the results add later shows its name alone.
const CASES = {
  'pages-500000p': 'a deflate bomb: a document that inflates past any limit',
  'px-8x1024mpx': 'a picture bomb: eight gigapixel pictures',
  'tables-10000': 'a document that converts for minutes',
  'png-g-32767': "a gray picture just inside LibreOffice's bound",
  'png-g-32768': "a gray picture just past LibreOffice's bound",
  'png-rgb-18918': "a colour picture just inside LibreOffice's bound",
  'png-rgb-18919': "a colour picture just past LibreOffice's bound",
  empty: 'an empty file',
  'random-1mb': 'random bytes',
  'random-10mb': 'more random bytes',
  tdf38575: "a document that hangs moalhaddar/docx-to-pdf's LibreOffice",
  pause: 'uploads that pause, then one that stalls halfway',
  egress: 'linked pictures aimed at a listener, public and private addresses',
}

/** The image's tag of a pinned reference, `name:tag@sha256:…`. */
const tag = (ref) => ref.split('@')[0].split(':').at(-1)

export function guide(results) {
  const f = speedFigures(results)
  const setup = cellSetup(results)
  const meta = results.meta
  const h = meta.host
  const { wins, ties } = gotenbergRows(results, `${SITE}/benchmarks/`)
  // One logical line a paragraph or item; guideBlock() wraps them, and leaves the tables' rows whole.
  return [
    BEGIN,
    '',
    '## Speed and memory',
    '',
    `Measured by \`make compare\` on ${at(results)}, on an ${v(h.cpu_model)} (${v(h.arch)}) virtual machine under ${v(h.hypervisor)}: ${setup.cpus} pinned vCPU, a pool of ${setup.pool} and ${setup.memory} of memory a service; a closed load of the typical reports ${setup.docs}; ${setup.conversions} conversions a service in each of ${setup.blocks} blocks, every block running each service once in a shuffled order, so a drift of the host moves them alike. Gotenberg ${v(tag(meta.images.Gotenberg))} ran twice, as it ships and with LibreOffice's restarts turned off. ${measuredGuide(meta)}`,
    '',
    `Each figure is the other service's over oxidestack/docx-to-pdf's, the ratio of the means ± its standard error, with the verdict of the 2σ test. A ratio measured inside one interleaved run holds from one host to the next better than an absolute figure; each row links the absolute figures on [Benchmarks](${SITE}/benchmarks/).`,
    '',
    ...ratioTable(f, (id, label) => `[${label}](${row(`ratio-${id}`)})`),
    '',
    'Where Gotenberg is ahead of oxidestack/docx-to-pdf in this run:',
    '',
    ...(wins.length ? wins.map((w) => `- ${w}`) : ['- nowhere']),
    '',
    'Where neither is ahead at 2σ:',
    '',
    ...(ties.length ? ties.map((t) => `- ${t}`) : ['- nowhere']),
    '',
    '## Hostile documents',
    '',
    `Each case went to each service once, each service as it ships, in a fresh container with ${v(meta.torture_memory.replace(/g$/, ' GiB'))} of memory on vCPUs ${v(meta.torture_cpus)}, waiting up to ${v(meta.cap)} s for the answer: the answer, its time, the container's memory peak and, for a picture, how many pictures reached the PDF. The answers are set by limits, and so repeat; the times and peaks are one run a side, with no σ.`,
    '',
    ...tortureTable(results, (c) => `[\`${v(c)}\`](${row(`torture-${c}`)})${CASES[c] ? `, ${CASES[c]}` : ''}`),
    '',
    `The soaks, thousands of conversions on one LibreOffice, are on [Benchmarks](${row('soaks')}).`,
    '',
    END,
  ].join('\n')
}

// ---------------------------------------------------------------------------------------------------------------

/** The README with its block replaced by `block` (placed after the title's paragraph when it has none). */
export function withBlock(readme, block) {
  const start = readme.indexOf(BEGIN)
  const end = readme.indexOf(END)
  if (start !== -1 && end !== -1) return readme.slice(0, start) + block + readme.slice(end + END.length)
  throw new Error(`README.md has no block between ${BEGIN} and ${END}`)
}

/** A file's findings: no block, a block that is not `block`, a figure outside it. */
function fileFindings(name, text, block) {
  const findings = []
  const start = text.indexOf(BEGIN)
  const end = text.indexOf(END)
  if (start === -1 || end === -1) findings.push(`${name} has no generated block`)
  else if (text.slice(start, end + END.length) !== block)
    findings.push(`${name}'s block is not what the results generate: run \`make evidence\``)
  for (const s of readmeStrays(text)) findings.push(`${name} states a figure outside the generated block: ${s}`)
  return findings
}

/**
 * Every finding of the check: a stray digit in a generated text, a stale block, a figure outside it. The comparison
 * guide is checked when it is given.
 */
export function check(results, readme, comparison) {
  const findings = []
  for (const s of strays(page(results))) findings.push(`the Benchmarks page has a number not read from the results: …${s}…`)
  for (const s of strays(headline(results))) findings.push(`the headline block has a number not read from the results: …${s}…`)
  for (const s of strays(guide(results))) findings.push(`the comparison block has a number not read from the results: …${s}…`)
  findings.push(...fileFindings('README.md', readme, readmeBlock(results)))
  if (comparison !== undefined) findings.push(...fileFindings('docs/guide/comparison.md', comparison, guideBlock(results)))
  return findings
}

function main() {
  const results = JSON.parse(readFileSync(RESULTS, 'utf8'))
  if (process.argv.includes('--write-readme')) {
    writeFileSync(README, withBlock(readFileSync(README, 'utf8'), readmeBlock(results)))
    writeFileSync(GUIDE, withBlock(readFileSync(GUIDE, 'utf8'), guideBlock(results)))
    console.log('README.md and docs/guide/comparison.md: the blocks rewritten from the results')
  }
  const findings = check(results, readFileSync(README, 'utf8'), readFileSync(GUIDE, 'utf8'))
  if (findings.length) {
    for (const f of findings) console.error(`evidence: ${f}`)
    process.exit(1)
  }
  mkdirSync(dirname(PAGE), { recursive: true })
  writeFileSync(PAGE, plain(page(results)))
  console.log(`evidence: the Benchmarks page and the two blocks from results.json (${results.meta.stamp})`)
}

if (process.argv[1] && fileURLToPath(import.meta.url) === process.argv[1]) {
  if (!existsSync(RESULTS)) throw new Error(`no results at ${RESULTS}`)
  main()
}
