// The tests of evidence.mjs: node --test scripts/*.test.mjs (site/build.sh runs them before the build).

import { test } from 'node:test'
import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import {
  BEGIN,
  RESULTS,
  SPEED,
  check,
  gotenbergRows,
  page,
  plain,
  readmeBlock,
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

test('the figures are the ones compare.py printed for the same run', () => {
  const f = speedFigures(results)
  const shown = (s, id, digits) => plain(record(f[s][id], digits))
  // tables.md of the committed run, row for row.
  assert.equal(shown('ours', 'throughput', 3), '0.896 ± 0.042 (3)')
  assert.equal(shown('gotenberg', 'cpu', 3), '1.276 ± 0.050 (3)')
  assert.equal(shown('g-norestart', 'p99', 2), '4.00 ± 0.07 (3)')
  assert.equal(shown('ours', 'peak', 0), '413 ± 0.3 (3)')
  assert.equal(shown('oracle', 'peak', 0), '967 ± 5 (3)')
  assert.equal(shown('oracle', 'ready', 2), '14.36 ± 0.96 (3)')
  assert.equal(shown('ours', 'written', 1), '28.2 ± 0.2 (3)')
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
  assert.match(check(results, edited).join('\n'), /headline block is not what the results generate/)
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
  assert.ok(block.includes('7 610 MiB'))
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
