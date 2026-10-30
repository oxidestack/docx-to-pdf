// Publish a new full `make compare` run: the results now served as the latest (public/benchmarks/results.json) move
// to public/benchmarks/earlier/<their stamp>.json, and the new run's results.json takes their place, as compare.py
// wrote it plus a final newline (the editorconfig gate wants one). Nothing else of either file changes, so each stays
// the raw output a reader can download and check the pages against. `make evidence RUN=<run>` runs it, then rewrites
// the README's and the comparison guide's blocks from the files.
//
//   node scripts/publish.mjs <.bench/compare/<stamp>/ or its results.json>
//
// It refuses, changing nothing, a run that is not a full one (QUICK=1 prints tables, it measures nothing on record),
// a run not newer than the latest, and a stamp already in earlier/. A run of another shape than the latest (another
// host, vCPU count, block count) is published like any other: evidence.mjs quotes ranges over the runs of the latest
// run's shape only, and lists the others on the Benchmarks page.

import { copyFileSync, existsSync, mkdirSync, readFileSync, renameSync, statSync, appendFileSync } from 'node:fs'
import { join } from 'node:path'
import { fileURLToPath } from 'node:url'
import { EARLIER, RESULTS } from './evidence.mjs'

/** The run's results file: the path itself, or the results.json in a run's directory. */
export function resultsOf(run) {
  return existsSync(run) && statSync(run).isDirectory() ? join(run, 'results.json') : run
}

/** The meta of a results file, or an error naming what is missing. */
function metaOf(path) {
  const meta = JSON.parse(readFileSync(path, 'utf8')).meta
  if (!meta || typeof meta.stamp !== 'string' || !/^\d{8}T\d{6}Z$/.test(meta.stamp))
    throw new Error(`${path} is not the results of a make compare run: no meta.stamp`)
  return meta
}

/**
 * Publish `run` as the latest results, moving the current latest into `earlier`. Returns where the old latest went
 * (null when there was none). Every check runs before the first file is touched.
 */
export function publish(run, { results = RESULTS, earlier = EARLIER } = {}) {
  const from = resultsOf(run)
  if (!existsSync(from)) throw new Error(`no results at ${from}`)
  const next = metaOf(from)
  if (next.mode !== 'full') throw new Error(`${from} is a ${next.mode} run; only a full run is published`)
  let moved = null
  if (existsSync(results)) {
    const latest = metaOf(results)
    if (next.stamp <= latest.stamp)
      throw new Error(`${from} (${next.stamp}) is not newer than the published run, ${latest.stamp}`)
    moved = join(earlier, `${latest.stamp}.json`)
    if (existsSync(moved)) throw new Error(`${moved} exists already`)
    mkdirSync(earlier, { recursive: true })
    renameSync(results, moved)
  }
  copyFileSync(from, results)
  if (!readFileSync(results, 'utf8').endsWith('\n')) appendFileSync(results, '\n')
  return moved
}

if (process.argv[1] && fileURLToPath(import.meta.url) === process.argv[1]) {
  const run = process.argv[2]
  if (!run) {
    console.error('usage: node scripts/publish.mjs <.bench/compare/<stamp>/ or its results.json>')
    process.exit(2)
  }
  try {
    const moved = publish(run)
    console.log(`publish: ${resultsOf(run)} is the latest run${moved ? `; the previous one moved to ${moved}` : ''}`)
  } catch (e) {
    console.error(`publish: ${e.message}`)
    process.exit(1)
  }
}
