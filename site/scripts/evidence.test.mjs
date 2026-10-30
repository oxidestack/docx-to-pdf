// The tests of evidence.mjs: node --test scripts/*.test.mjs (site/build.sh runs them before the build).

import { test } from 'node:test'
import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import {
  BEGIN,
  RESULTS,
  bombs,
  headline,
  loadEarlier,
  ratioOver,
  runsOf,
  span,
  tortureCell,
  SPEED,
  check,
  gotenbergRows,
  guide,
  guideBlock,
  page,
  plain,
  readmeBlock,
  release,
  readmeStrays,
  record,
  speedFigures,
  strays,
  v,
  withBlock,
  wrap,
} from './evidence.mjs'

const results = JSON.parse(readFileSync(RESULTS, 'utf8'))
// A README of the repository's shape with the block as the results write it. The repository's own README is held by
// the build's check itself (evidence.mjs, run by the prebuild), which names what it found.
const shell = `# docx-to-pdf\n\n${BEGIN}\n<!-- benchmarks:end -->\n\nAn HTTP service.\n\n## Quick start\n\n\`\`\`sh\ndocker run -p 8080:8080 x\n\`\`\`\n`
const readme = withBlock(shell, readmeBlock(results))
// The comparison guide's shape: prose with settings in code, then the generated block.
const guideShell = `# Compared\n\nThe timeout is \`180s\` by default.\n\n${BEGIN}\n<!-- benchmarks:end -->\n\n## Rendering\n`
const comparison = withBlock(guideShell, guideBlock(results))

test('the figures are the ones compare.py printed for the same run', () => {
  const f = speedFigures(results)
  const shown = (s, id, digits) => plain(record(f[s][id], digits))
  // tables.md of the committed run, row for row.
  assert.equal(shown('ours', 'throughput', 3), '0.942 ± 0.023 (3)')
  assert.equal(shown('gotenberg', 'cpu', 3), '1.216 ± 0.025 (3)')
  assert.equal(shown('g-norestart', 'p99', 2), '4.01 ± 0.22 (3)')
  assert.equal(shown('ours', 'peak', 0), '413 ± 0.3 (3)')
  assert.equal(shown('oracle', 'peak', 0), '938 ± 50 (3)')
  assert.equal(shown('oracle', 'ready', 2), '13.38 ± 0.93 (3)')
  assert.equal(shown('ours', 'written', 1), '28.3 ± 0.2 (3)')
})

test('a marked value passes, a digit typed into a template does not', () => {
  assert.deepEqual(strays(`uses ${v('1.20')}× the CPU, p99 within 2σ`), [])
  assert.equal(strays(`uses ${v('1.20')}× the CPU, about 2.5× the memory`).length, 2)
  assert.equal(strays(`<rect width="120"/><text>${v('7 610')} MiB</text>`).length, 0)
  assert.equal(strays('<text>7 610 MiB</text>').length, 4)
})

test('the generated page and block carry no number the results do not', () => {
  assert.deepEqual(strays(page(results)), [])
  assert.deepEqual(check(results, readme), [])
})

test('a number edited in the README block fails the check', () => {
  const edited = readme.replace(/Gotenberg uses \d\.\d\d/, 'Gotenberg uses 1.99')
  assert.notEqual(edited, readme)
  assert.match(check(results, edited).join('\n'), /README\.md's block is not what the results generate/)
})

test('a figure typed into the README outside the block fails the check, a port in code does not', () => {
  const typed = readme.replace('## Quick start', 'It uses half the memory, 2.1× less.\n\n## Quick start')
  assert.match(check(results, typed).join('\n'), /outside the generated block: 2.1×/)
  assert.deepEqual(readmeStrays('```sh\ndocker run -p 8080:8080 x\n```\nRFC 9457, version 2.1.0-13.\n'), [])
  assert.equal(readmeStrays('It starts in 0.6 s.\n').length, 1)
})

test('the block rewrites in place and wraps to the width without splitting a number or a link', () => {
  const block = readmeBlock(results)
  assert.equal(withBlock(readme, block), readme)
  for (const line of block.split('\n')) if (!line.startsWith('<!--')) assert.ok(line.length <= 120, line)
  assert.ok(block.includes('7 609–8 076 MiB'))
  assert.doesNotMatch(block, /\[the\n/)
  assert.equal(wrap(`- ${'word '.repeat(30)}7 610 MiB`).split('\n')[1].trim().startsWith('word'), true)
})

test('the page shows the rows where Gotenberg is ahead', () => {
  const { wins } = gotenbergRows(results)
  assert.ok(wins.length > 0)
  const p = plain(page(results))
  assert.match(p, /## Where Gotenberg wins/)
  for (const w of wins) assert.ok(p.includes(plain(w)))
  for (const [, name] of SPEED) assert.ok(p.includes(name))
})

test('the README block says the one sentence of the positioning, and no more', () => {
  const block = readmeBlock(results)
  assert.match(block.replace(/\n> /g, ' '), /than Gotenberg and moalhaddar\/docx-to-pdf on the same documents/)
  assert.doesNotMatch(block, /anything else|hang it/)
  assert.match(block.replace(/\n/g, " "), release(results.meta) ? /The image measured is release/ : /before the heap trim/)
})

test('the comparison guide carries a generated block, held like the README', () => {
  assert.deepEqual(strays(guide(results)), [])
  assert.deepEqual(check(results, readme, comparison), [])
  const edited = comparison.replace(/\d\.\d\d ± /, '1.99 ± ')
  assert.notEqual(edited, comparison)
  assert.match(check(results, readme, edited).join('\n'), /comparison\.md's block is not what the results generate/)
  const typed = comparison.replace('## Rendering', 'Memory is about half, 1.9× less.\n\n## Rendering')
  assert.match(check(results, readme, typed).join('\n'), /comparison\.md states a figure outside the generated block: 1\.9×/)
})

test('the guide block links each row to the Benchmarks page and keeps table rows whole', () => {
  const block = guideBlock(results)
  for (const id of ['ratio-cpu', 'ratio-peak', 'torture-pages-500000p']) assert.ok(block.includes(`/benchmarks/#${id}`), id)
  for (const line of block.split('\n')) if (line.startsWith('|')) assert.ok(line.endsWith('|'), line)
  assert.match(plain(block), /## Hostile documents/)
})

test('a published release pulled by digest is named as the code measured; an image the command built is not', () => {
  assert.equal(release(results.meta), '1.0.0')
  assert.doesNotMatch(readmeBlock(results) + guide(results) + page(results), /before the heap trim|not a release image/)
  const built = { ...results, meta: { ...results.meta, image: 'docx-to-pdf:compare' } }
  assert.equal(release(built.meta), null)
  assert.match(readmeBlock(built), /measured on the code before the heap trim/)
  assert.deepEqual(strays(page(built)), [])
})

// The earlier full runs committed beside the latest: the README and the guide quote ranges over all of them.
const earlier = loadEarlier()

test('the earlier runs are of the latest run\'s shape, and earlier', () => {
  assert.ok(earlier.length >= 2)
  const runs = runsOf(results, earlier)
  assert.equal(runs.at(-1), results)
  assert.ok(runs.slice(0, -1).every((r, i, a) => i === 0 || a[i - 1].meta.stamp < r.meta.stamp))
  const other = { ...earlier[0], meta: { ...earlier[0].meta, blocks: 5 } }
  assert.throws(() => runsOf(results, [other]), /differs from the latest in meta\.blocks/)
  assert.throws(() => runsOf(results, [results]), /is not earlier/)
})

test('a range reads one figure when its ends read alike, and every part is marked', () => {
  assert.equal(plain(span([1.651, 1.757], (x) => x.toFixed(2))), '1.65–1.76')
  assert.equal(plain(span([235, 235])), '235')
  assert.deepEqual(strays(span([235, 237])), [])
  assert.match(plain(ratioOver([results], 'gotenberg', 'ours', 'peak')), /^\d\.\d\d ± [\d.]+×$/)
})

test('the README quotes each ratio as the range over the runs, and both bombs alike', () => {
  const block = readmeBlock(results, earlier).replace(/\n\s*/g, ' ')
  const runs = runsOf(results, earlier)
  const ends = (a, b, k) => plain(ratioOver(runs, a, b, k))
  for (const [a, b, k] of [['gotenberg', 'ours', 'cpu'], ['gotenberg', 'ours', 'peak'], ['g-norestart', 'ours', 'peak'], ['ours', 'oracle', 'throughput']])
    assert.ok(block.includes(ends(a, b, k)), `${a}/${b} ${k}`)
  assert.match(ends('gotenberg', 'ours', 'peak'), /^\d\.\d\d–\d\.\d\d×$/)
  const { cells } = bombs(runs)
  const peaks = cells.ours.map((c) => c.peak)
  assert.ok(block.includes(`at a ${Math.min(...peaks)}–${Math.max(...peaks)} MiB peak`))
  assert.match(block, new RegExp(`the same answers in each of the ${runs.length} runs`))
  assert.deepEqual(strays(headline(results, earlier)), [])
  assert.deepEqual(check(results, withBlock(shell, readmeBlock(results, earlier)), withBlock(guideShell, guideBlock(results, earlier)), earlier), [])
})

test('the guide gives a range with each run\'s verdict, and a hostile cell\'s time and peak over the runs', () => {
  const block = plain(guideBlock(results, earlier))
  assert.match(block, /ahead in each of the \d runs/)
  const runs = runsOf(results, earlier)
  const cell = plain(tortureCell(runs, 'pages-500000p', 'ours'))
  assert.match(cell, /^413 in [\d.]+(–[\d.]+)? s; peak \d+(–\d+)? MiB; alongside/)
  assert.match(plain(tortureCell(runs, 'egress', 'moalhaddar')), /\(the latest run\)$/)
  assert.match(block.replace(/\n/g, ' '), /one soak, a level measured, not a trend/)
  assert.doesNotMatch(block, /\bflat\b/)
  assert.deepEqual(strays(guide(results, earlier)), [])
  assert.deepEqual(strays(page(results, earlier)), [])
  assert.match(plain(page(results, earlier)), /## Earlier runs/)
})
