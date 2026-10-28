// The bench's load: POST /pdf with the documents of one workload, in the
// workload model MODE names. Run by tools/bench/bench.py in grafana/k6 2.3.0 with
// K6_AUTO_EXTENSION_RESOLUTION=false (k6/experimental/fs is built in; resolution would try to fetch it).
//
//   TARGET     the service, e.g. http://service:8080
//   DOCS       name=path,... inside the container; requests take them in turn
//   MODE       open    constant-arrival-rate: RATE requests/s for DURATION (latency against offered load)
//              closed  constant-vus: VUS clients back to back for DURATION (saturation, the USL fit)
//              count   shared-iterations: VUS clients back to back until ITERATIONS requests (a closed load whose
//                      length follows the precision wanted, not the clock)
//              ramp    ramping-arrival-rate: STAGES "rate:duration,..."; aborts on the stopping criteria
//              warmup  shared-iterations: ITERATIONS by VUS clients (discarded)
//   TIMEOUT    a request's timeout (default 900s, above any conversion timeout plus the pool wait)
//   MAX_VUS    the open models' client cap (default 200); a request that finds none free is a dropped iteration
//   URL_PATH   the conversion route (default /pdf); FIELD the multipart file field (default document)
import exec from 'k6/execution';
import { open as openFile, SeekMode } from 'k6/experimental/fs';
import http from 'k6/http';

const env = __ENV;
const target = env.TARGET || 'http://service:8080';
const timeout = env.TIMEOUT || '900s';
const urlPath = env.URL_PATH || '/pdf';
const field = env.FIELD || 'document';
const maxVUs = parseInt(env.MAX_VUS || '200', 10);
const DOCX = 'application/vnd.openxmlformats-officedocument.wordprocessingml.document';

const docs = [];
for (const pair of (env.DOCS || '').split(',').filter(Boolean)) {
  const [name, path] = pair.split('=');
  // Opened once per test run and shared by every VU (k6's fs module), so a large document is in memory once; each
  // request reads its own copy.
  const file = await openFile(path);
  docs.push({ name, path, file, size: (await file.stat()).size });
}
if (docs.length === 0) throw new Error('DOCS names no document');

// A rate as an integer per time unit, so fractional rates (0.05/s) work.
function rate(perSecond) {
  return { rate: Math.max(1, Math.round(parseFloat(perSecond) * 1000)), timeUnit: '1000s' };
}

function scenario() {
  switch (env.MODE || 'open') {
    case 'open':
      return {
        executor: 'constant-arrival-rate',
        ...rate(env.RATE),
        duration: env.DURATION,
        preAllocatedVUs: parseInt(env.PRE_VUS || '20', 10),
        maxVUs,
      };
    case 'closed':
      return { executor: 'constant-vus', vus: parseInt(env.VUS, 10), duration: env.DURATION };
    case 'ramp': {
      const stages = env.STAGES.split(',').map((s) => {
        const [r, d] = s.split(':');
        return { target: rate(r).rate, duration: d };
      });
      return {
        executor: 'ramping-arrival-rate',
        startRate: stages[0].target,
        timeUnit: '1000s',
        stages,
        preAllocatedVUs: parseInt(env.PRE_VUS || '20', 10),
        maxVUs,
      };
    }
    case 'count':
      return {
        executor: 'shared-iterations',
        vus: parseInt(env.VUS, 10),
        iterations: parseInt(env.ITERATIONS, 10),
        maxDuration: env.DURATION || '60m',
      };
    case 'warmup':
      return {
        executor: 'shared-iterations',
        vus: parseInt(env.VUS, 10),
        iterations: parseInt(env.ITERATIONS, 10),
        maxDuration: env.DURATION || '60m',
      };
    default:
      throw new Error(`unknown MODE ${env.MODE}`);
  }
}

// Stopping criteria for a ramp: errors over 1 % or any dropped request end it; the rest
// (p99, backlog, OOM) are read from the data afterwards.
const thresholds =
  env.MODE === 'ramp'
    ? {
        http_req_failed: [{ threshold: 'rate<0.01', abortOnFail: true, delayAbortEval: '60s' }],
        dropped_iterations: [{ threshold: 'count<1', abortOnFail: true }],
      }
    : {};

export const options = {
  scenarios: { load: scenario() },
  thresholds,
  discardResponseBodies: true,
  summaryTrendStats: ['min', 'med', 'p(90)', 'p(95)', 'p(99)', 'max', 'count'],
  setupTimeout: '10m',
};

export default async function () {
  const doc = docs[exec.scenario.iterationInTest % docs.length];
  const buffer = new Uint8Array(doc.size);
  await doc.file.seek(0, SeekMode.Start);
  let read = 0;
  while (read < doc.size) {
    const n = await doc.file.read(buffer.subarray(read));
    if (n === null) break;
    read += n;
  }
  http.post(
    `${target}${urlPath}`,
    { [field]: http.file(buffer.buffer, `${doc.name}.docx`, DOCX) },
    { timeout, tags: { doc: doc.name }, responseCallback: http.expectedStatuses(200) },
  );
}

export function handleSummary(data) {
  return { '/out/k6-summary.json': JSON.stringify(data, null, 1) };
}
