// The tests of publish.mjs: node --test scripts/*.test.mjs (site/build.sh runs them before the build).

import { test } from 'node:test'
import assert from 'node:assert/strict'
import { existsSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { publish } from './publish.mjs'

/** A site's benchmarks directory with `latest` published, and a run directory per further stamp, in a fresh tmp. */
function site(latest, ...runs) {
  const root = mkdtempSync(join(tmpdir(), 'publish-'))
  const paths = { results: join(root, 'benchmarks', 'results.json'), earlier: join(root, 'benchmarks', 'earlier') }
  mkdirSync(join(root, 'benchmarks'))
  if (latest) writeFileSync(paths.results, `${JSON.stringify({ meta: { stamp: latest, mode: 'full' } })}\n`)
  const dirs = runs.map(([stamp, mode = 'full']) => {
    const dir = join(root, 'compare', stamp)
    mkdirSync(dir, { recursive: true })
    // compare.py writes json.dump's output: no final newline.
    writeFileSync(join(dir, 'results.json'), JSON.stringify({ meta: { stamp, mode }, speed: { x: 1 } }, null, 1))
    return dir
  })
  return { paths, dirs }
}

const stampOf = (path) => JSON.parse(readFileSync(path, 'utf8')).meta.stamp

test('a new full run moves the latest into earlier/ under its stamp and takes its place, bytes kept', () => {
  const { paths, dirs } = site('20261029T171628Z', ['20261105T090000Z'])
  const before = readFileSync(paths.results, 'utf8')
  const moved = publish(dirs[0], paths)
  assert.equal(moved, join(paths.earlier, '20261029T171628Z.json'))
  assert.equal(readFileSync(moved, 'utf8'), before)
  const written = readFileSync(join(dirs[0], 'results.json'), 'utf8')
  assert.equal(readFileSync(paths.results, 'utf8'), `${written}\n`)
  assert.equal(stampOf(paths.results), '20261105T090000Z')
})

test('the run is given as its directory or its results.json, and a first run moves nothing', () => {
  const { paths, dirs } = site(null, ['20261105T090000Z'])
  assert.equal(publish(join(dirs[0], 'results.json'), paths), null)
  assert.equal(stampOf(paths.results), '20261105T090000Z')
  assert.equal(existsSync(paths.earlier), false)
})

test('runs published one after another pile up in earlier/, each under its own stamp', () => {
  const { paths, dirs } = site('20261029T171628Z', ['20261105T090000Z'], ['20261106T090000Z'])
  publish(dirs[0], paths)
  publish(dirs[1], paths)
  assert.deepEqual(readdirSync(paths.earlier).sort(), ['20261029T171628Z.json', '20261105T090000Z.json'])
  assert.equal(stampOf(paths.results), '20261106T090000Z')
})

test('a quick run, a run not newer, a stamp already in earlier/ and a file of another kind are refused untouched', () => {
  const { paths, dirs } = site('20261029T171628Z', ['20261105T090000Z', 'quick'], ['20261028T100607Z'], ['20261107T090000Z'])
  const before = readFileSync(paths.results, 'utf8')
  assert.throws(() => publish(dirs[0], paths), /is a quick run/)
  assert.throws(() => publish(dirs[1], paths), /is not newer than the published run, 20261029T171628Z/)
  mkdirSync(paths.earlier)
  writeFileSync(join(paths.earlier, '20261029T171628Z.json'), '{}\n')
  assert.throws(() => publish(dirs[2], paths), /exists already/)
  const other = join(dirs[2], 'tables.json')
  writeFileSync(other, '{"rows": []}')
  assert.throws(() => publish(other, paths), /no meta\.stamp/)
  assert.throws(() => publish(join(dirs[2], 'nothing'), paths), /no results at/)
  assert.equal(readFileSync(paths.results, 'utf8'), before)
  assert.deepEqual(readdirSync(paths.earlier), ['20261029T171628Z.json'])
})
