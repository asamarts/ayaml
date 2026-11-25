import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { SITE_ROOT } from './site';

const DATA_DIR = join(SITE_ROOT, 'src', 'data', 'benchmarks');

export interface SpeedRow {
  schema_version: number;
  scenario: string;
  operation: string;
  implementation: string;
  bytes: number;
  fixture_sha256: string;
  median_ns: number;
  mean_ns: number;
  stddev_ns: number;
  throughput_mib_s: number;
  rounds: number;
}

export interface MemoryRow {
  schema_version: number;
  scenario: string;
  operation: string;
  implementation: string;
  bytes: number;
  fixture_sha256: string;
  peak_bytes: number;
  live_bytes: number;
  allocations: number;
  total_allocated_bytes: number;
  peak_to_input_ratio: number;
}

export interface BenchmarkFingerprint {
  schema_version: number;
  os: string;
  arch: string;
  cpu: string;
  cpu_count: number;
  cpu_pin: number;
  governor: string;
  rustc: string;
  fixture_manifest_sha256: string;
  public_git_sha: string;
  baseline_v023_git_sha: string;
  captured_date: string;
  speed_rows: number;
  memory_rows: number;
}

function readJsonl<T>(filename: string): T[] {
  const raw = readFileSync(join(DATA_DIR, filename), 'utf8');
  return raw
    .split('\n')
    .map((l) => l.trim())
    .filter(Boolean)
    .map((l) => JSON.parse(l) as T);
}

export const fingerprint: BenchmarkFingerprint = JSON.parse(
  readFileSync(join(DATA_DIR, 'fingerprint.json'), 'utf8'),
) as BenchmarkFingerprint;

export const speedRows: SpeedRow[] = readJsonl<SpeedRow>('speed.jsonl');
export const memoryRows: MemoryRow[] = readJsonl<MemoryRow>('memory.jsonl');
export const v023SpeedRows: SpeedRow[] = readJsonl<SpeedRow>('v023-speed.jsonl');
export const v023MemoryRows: MemoryRow[] = readJsonl<MemoryRow>('v023-memory.jsonl');

// Build-time data integrity assertions: fail fast if pinned files ever drift.
if (speedRows.length === 0 || speedRows.length !== fingerprint.speed_rows) {
  throw new Error(
    `bench.ts: speed.jsonl has ${speedRows.length} rows, fingerprint expects ${fingerprint.speed_rows}`,
  );
}
if (memoryRows.length === 0 || memoryRows.length !== fingerprint.memory_rows) {
  throw new Error(
    `bench.ts: memory.jsonl has ${memoryRows.length} rows, fingerprint expects ${fingerprint.memory_rows}`,
  );
}
if (v023SpeedRows.length === 0) {
  throw new Error('bench.ts: v023-speed.jsonl contains no rows');
}
if (v023MemoryRows.length === 0) {
  throw new Error('bench.ts: v023-memory.jsonl contains no rows');
}

const speedMap = new Map<string, SpeedRow>();
for (const r of speedRows) {
  speedMap.set(`${r.implementation}|${r.operation}|${r.scenario}`, r);
}

const memoryMap = new Map<string, MemoryRow>();
const memoryScenarioSet = new Set<string>();
for (const r of memoryRows) {
  memoryMap.set(`${r.implementation}|${r.operation}|${r.scenario}`, r);
  memoryScenarioSet.add(r.scenario);
}

const v023SpeedMap = new Map<string, SpeedRow>();
for (const r of v023SpeedRows) {
  v023SpeedMap.set(`${r.operation}|${r.scenario}`, r);
}

const v023MemoryMap = new Map<string, MemoryRow>();
for (const r of v023MemoryRows) {
  v023MemoryMap.set(`${r.operation}|${r.scenario}`, r);
}

export function getSpeed(impl: string, op: string, scenario: string): SpeedRow | undefined {
  return speedMap.get(`${impl}|${op}|${scenario}`);
}

export function getMemory(impl: string, op: string, scenario: string): MemoryRow | undefined {
  return memoryMap.get(`${impl}|${op}|${scenario}`);
}

function mustSpeed(impl: string, op: string, scenario: string): SpeedRow {
  const row = getSpeed(impl, op, scenario);
  if (!row) throw new Error(`bench.ts: missing speed row ${impl}|${op}|${scenario}`);
  return row;
}

function mustMemory(impl: string, op: string, scenario: string): MemoryRow {
  const row = getMemory(impl, op, scenario);
  if (!row) throw new Error(`bench.ts: missing memory row ${impl}|${op}|${scenario}`);
  return row;
}

export function geomean(values: number[]): number {
  const pos = values.filter((v) => Number.isFinite(v) && v > 0);
  if (pos.length === 0) return Number.NaN;
  const logSum = pos.reduce((acc, v) => acc + Math.log(v), 0);
  return Math.exp(logSum / pos.length);
}

export interface ScenarioMeta {
  id: string;
  category: 'domain' | 'boundary' | 'scaling' | 'stress';
  label: string;
  bytes: number;
}

const SCENARIO_SPECS: Array<Omit<ScenarioMeta, 'bytes'>> = [
  { id: 'frontmatter-tiny', category: 'domain', label: 'Tiny frontmatter header' },
  { id: 'ci-workflow', category: 'domain', label: 'CI workflow with block scripts' },
  { id: 'container-compose', category: 'domain', label: 'Container compose (anchors + merges)' },
  { id: 'cluster-manifests', category: 'domain', label: 'Multi-doc cluster manifests' },
  { id: 'openapi-spec', category: 'domain', label: 'OpenAPI specification' },
  { id: 'lockfile-wide', category: 'domain', label: 'Wide dependency lockfile' },
  { id: 'escape-routes', category: 'domain', label: 'Escape-heavy route table' },
  { id: 'json-in-yaml', category: 'domain', label: 'Strict JSON-in-YAML payload' },
  { id: 'unicode-multibyte', category: 'domain', label: 'Multi-byte Unicode document' },
  { id: 'boundary-4kb-below', category: 'boundary', label: 'Structured map below 4 KiB' },
  { id: 'boundary-4kb-above', category: 'boundary', label: 'Structured map above 4 KiB' },
  { id: 'scale-map-10kb', category: 'scaling', label: 'Block mapping (10 KiB)' },
  { id: 'scale-map-100kb', category: 'scaling', label: 'Block mapping (100 KiB)' },
  { id: 'scale-seq-10kb', category: 'scaling', label: 'Block sequence (10 KiB)' },
  { id: 'scale-seq-100kb', category: 'scaling', label: 'Block sequence (100 KiB)' },
  { id: 'scale-openapi-10kb', category: 'scaling', label: 'Nested OpenAPI (10 KiB)' },
  { id: 'scale-openapi-100kb', category: 'scaling', label: 'Nested OpenAPI (100 KiB)' },
  { id: 'depth-64', category: 'stress', label: 'Deep flow nesting (depth 64)' },
  { id: 'aliases-1000', category: 'stress', label: 'Repeated alias fan-out (1,000)' },
  { id: 'short-seq-20000', category: 'stress', label: '20,000 short sequence items' },
  { id: 'short-map-20000', category: 'stress', label: '20,000 short mapping entries' },
  { id: 'short-explicit-20000', category: 'stress', label: '20,000 explicit-key entries' },
  { id: 'large-scalar-1mb', category: 'stress', label: '1 MiB block scalar' },
];

export const ALL_SCENARIOS: ScenarioMeta[] = SCENARIO_SPECS.map((spec) => {
  const row = getSpeed('ayaml', 'events', spec.id) ?? getMemory('ayaml', 'events', spec.id);
  if (!row) throw new Error(`bench.ts: missing scenario ${spec.id} in pinned data`);
  return { ...spec, bytes: row.bytes };
});

export interface PeerComparisonRow {
  impl: string;
  op: string;
  label: string;
  lang: 'Rust' | 'C' | 'C++';
  safety: string;
  sharedScenarios: number;
  memSharedScenarios: number;
  speedRatio: number; // peer_time / ayaml_time (>1.0 means ayaml is faster)
  peakRatio: number | null;
  allocRatio: number | null;
  isAyaml: boolean;
  isUnsafeArena?: boolean;
}

export function computeTierComparison(
  baseImpl: string,
  baseOp: string,
  peers: Array<{
    impl: string;
    op: string;
    label: string;
    lang: 'Rust' | 'C' | 'C++';
    safety: string;
    isAyaml?: boolean;
    isUnsafeArena?: boolean;
  }>,
): PeerComparisonRow[] {
  const rows = peers.map((peer) => {
    const timeRatios: number[] = [];
    const peakRatios: number[] = [];
    const allocRatios: number[] = [];

    for (const s of ALL_SCENARIOS) {
      const bSpeed = getSpeed(baseImpl, baseOp, s.id);
      const pSpeed = getSpeed(peer.impl, peer.op, s.id);
      if (bSpeed && pSpeed && bSpeed.median_ns > 0 && pSpeed.median_ns > 0) {
        timeRatios.push(pSpeed.median_ns / bSpeed.median_ns);
      }

      const bMem = getMemory(baseImpl, baseOp, s.id);
      const pMem = getMemory(peer.impl, peer.op, s.id);
      if (bMem && pMem && bMem.peak_bytes > 0 && pMem.peak_bytes > 0) {
        peakRatios.push(pMem.peak_bytes / bMem.peak_bytes);
      }
      if (bMem && pMem && bMem.allocations > 0 && pMem.allocations > 0) {
        allocRatios.push(pMem.allocations / bMem.allocations);
      }
    }

    if (timeRatios.length === 0) {
      throw new Error(
        `bench.ts: no shared speed scenarios between ${baseImpl}/${baseOp} and ${peer.impl}/${peer.op}`,
      );
    }

    return {
      impl: peer.impl,
      op: peer.op,
      label: peer.label,
      lang: peer.lang,
      safety: peer.safety,
      sharedScenarios: timeRatios.length,
      memSharedScenarios: peakRatios.length,
      speedRatio: geomean(timeRatios),
      peakRatio: peakRatios.length > 0 ? geomean(peakRatios) : null,
      allocRatio: allocRatios.length > 0 ? geomean(allocRatios) : null,
      isAyaml: Boolean(peer.isAyaml),
      isUnsafeArena: peer.isUnsafeArena,
    };
  });
  return rows.sort((a, b) => a.speedRatio - b.speedRatio);
}

export const TIER2_PEERS = computeTierComparison('ayaml', 'value', [
  {
    impl: 'rapidyaml',
    op: 'resolve-typed',
    label: 'rapidyaml (C++ arena)',
    lang: 'C++',
    safety: 'Unsafe C++ flat arena (no Rust DOM)',
    isUnsafeArena: true,
  },
  {
    impl: 'ayaml',
    op: 'value',
    label: 'ayaml (Value DOM)',
    lang: 'Rust',
    safety: 'Safe Rust (#![forbid(unsafe_code)])',
    isAyaml: true,
  },
  {
    impl: 'ayaml',
    op: 'compat-serde',
    label: 'ayaml (compat serde)',
    lang: 'Rust',
    safety: 'Safe Rust (libyaml-compat)',
    isAyaml: true,
  },
  { impl: 'fkyaml', op: 'parse', label: 'fkYAML', lang: 'C++', safety: 'C++ DOM parser' },
  {
    impl: 'ayaml',
    op: 'serde-value',
    label: 'ayaml (serde Value)',
    lang: 'Rust',
    safety: 'Safe Rust Serde',
    isAyaml: true,
  },
  { impl: 'yaml-rust2', op: 'parse', label: 'yaml-rust2', lang: 'Rust', safety: 'Safe Rust DOM' },
  { impl: 'noyalib', op: 'value', label: 'noyalib', lang: 'Rust', safety: 'Rust YAML DOM' },
  { impl: 'saphyr', op: 'parse', label: 'saphyr', lang: 'Rust', safety: 'Safe Rust DOM' },
  { impl: 'fast-yaml-core', op: 'parse', label: 'fast-yaml-core', lang: 'Rust', safety: 'Rust DOM' },
  {
    impl: 'yaml-serde',
    op: 'value',
    label: 'yaml-serde',
    lang: 'Rust',
    safety: 'Unsafe C-transpiled libyaml-rs',
  },
  {
    impl: 'serde-saphyr',
    op: 'serde-json',
    label: 'serde-saphyr',
    lang: 'Rust',
    safety: 'Safe Rust Serde',
  },
]);

export const TIER1_PEERS = computeTierComparison('ayaml', 'events', [
  {
    impl: 'ayaml',
    op: 'scan',
    label: 'ayaml (scanner only)',
    lang: 'Rust',
    safety: 'Safe Rust tokenizer',
    isAyaml: true,
  },
  {
    impl: 'rapidyaml',
    op: 'parse-in-place',
    label: 'rapidyaml (parse-in-place)',
    lang: 'C++',
    safety: 'Unsafe C++ mutable in-place arena',
    isUnsafeArena: true,
  },
  {
    impl: 'rapidyaml',
    op: 'parse-reuse',
    label: 'rapidyaml (parse-reuse)',
    lang: 'C++',
    safety: 'Unsafe C++ reused arena',
    isUnsafeArena: true,
  },
  {
    impl: 'rapidyaml',
    op: 'parse-arena',
    label: 'rapidyaml (parse-arena)',
    lang: 'C++',
    safety: 'Unsafe C++ flat arena',
    isUnsafeArena: true,
  },
  {
    impl: 'ayaml',
    op: 'events',
    label: 'ayaml (spanned events)',
    lang: 'Rust',
    safety: 'Safe Rust YAML 1.2.2',
    isAyaml: true,
  },
  { impl: 'libyaml', op: 'events', label: 'libyaml (events)', lang: 'C', safety: 'Unsafe C parser' },
  { impl: 'libyaml', op: 'load', label: 'libyaml (load)', lang: 'C', safety: 'Unsafe C document' },
  {
    impl: 'ayaml',
    op: 'spanned',
    label: 'ayaml (SpannedTree)',
    lang: 'Rust',
    safety: 'Safe Rust spanned tree',
    isAyaml: true,
  },
  { impl: 'libfyaml', op: 'events', label: 'libfyaml (events)', lang: 'C', safety: 'Unsafe C parser' },
  { impl: 'libfyaml', op: 'doc', label: 'libfyaml (doc)', lang: 'C', safety: 'Unsafe C document' },
]);

export const TYPED_SCENARIOS = [
  { id: 'frontmatter-tiny', label: 'Tiny frontmatter' },
  { id: 'boundary-4kb-below', label: 'Below 4 KiB boundary' },
  { id: 'boundary-4kb-above', label: 'Above 4 KiB boundary' },
] as const;

export interface TypedDecodeRow {
  scenario: string;
  impl: string;
  op: string;
  label: string;
  medianUs: number;
  throughput: number;
  peakBytes: number;
  liveBytes: number;
  allocations: number;
  isAyaml: boolean;
}

export function getTypedDecodeRows(scenario: string): TypedDecodeRow[] {
  const specs = [
    { impl: 'ayaml', op: 'borrowed', label: 'ayaml (from_str_borrowed)', isAyaml: true },
    { impl: 'ayaml', op: 'typed', label: 'ayaml (from_str owned)', isAyaml: true },
    { impl: 'yaml-serde', op: 'typed', label: 'yaml-serde (owned)', isAyaml: false },
    { impl: 'noyalib', op: 'borrowed', label: 'noyalib (borrowed)', isAyaml: false },
    { impl: 'noyalib', op: 'typed', label: 'noyalib (owned)', isAyaml: false },
    { impl: 'serde-saphyr', op: 'typed', label: 'serde-saphyr (owned)', isAyaml: false },
  ];
  const out: TypedDecodeRow[] = [];
  for (const spec of specs) {
    const sp = getSpeed(spec.impl, spec.op, scenario);
    const mm = getMemory(spec.impl, spec.op, scenario);
    if (!sp || !mm) continue;
    out.push({
      scenario,
      impl: spec.impl,
      op: spec.op,
      label: spec.label,
      medianUs: sp.median_ns / 1000,
      throughput: sp.throughput_mib_s,
      peakBytes: mm.peak_bytes,
      liveBytes: mm.live_bytes,
      allocations: mm.allocations,
      isAyaml: spec.isAyaml,
    });
  }
  if (out.length === 0) {
    throw new Error(`bench.ts: no typed decode rows found for scenario ${scenario}`);
  }
  return out.sort((a, b) => a.medianUs - b.medianUs);
}

export const EMIT_SCENARIOS = ['frontmatter-tiny', 'ci-workflow', 'openapi-spec'] as const;

export interface EmitRow {
  scenario: string;
  impl: string;
  label: string;
  medianUs: number;
  throughput: number;
  isAyaml: boolean;
  isUnsafeArena?: boolean;
}

export function getEmitRows(scenario: string): EmitRow[] {
  const specs = [
    { impl: 'rapidyaml', label: 'rapidyaml (C++ arena)', isAyaml: false, isUnsafeArena: true },
    { impl: 'ayaml', label: 'ayaml (safe Rust)', isAyaml: true },
    { impl: 'fkyaml', label: 'fkYAML (C++)', isAyaml: false },
    { impl: 'noyalib', label: 'noyalib (Rust)', isAyaml: false },
    { impl: 'yaml-serde', label: 'yaml-serde (libyaml)', isAyaml: false },
  ];
  const out: EmitRow[] = [];
  for (const spec of specs) {
    const sp = getSpeed(spec.impl, 'emit', scenario);
    if (!sp) continue;
    out.push({
      scenario,
      impl: spec.impl,
      label: spec.label,
      medianUs: sp.median_ns / 1000,
      throughput: sp.throughput_mib_s,
      isAyaml: spec.isAyaml,
      isUnsafeArena: spec.isUnsafeArena,
    });
  }
  if (out.length === 0) {
    throw new Error(`bench.ts: no emit rows found for scenario ${scenario}`);
  }
  return out.sort((a, b) => b.throughput - a.throughput);
}

export interface ProgressRow {
  label: string;
  scenario: string;
  op: string;
  v023Mib: number;
  curMib: number;
  speedup: number;
  peakRatio: number | null; // v0.2.3 peak / current peak (>1.0 means lower peak heap now)
}

export function getProgressSinceV023(): ProgressRow[] {
  const picks = [
    { label: 'Emit CI workflow', scenario: 'ci-workflow', op: 'emit' },
    { label: 'Emit OpenAPI spec', scenario: 'openapi-spec', op: 'emit' },
    { label: 'Emit frontmatter', scenario: 'frontmatter-tiny', op: 'emit' },
    { label: 'Events: 20k short map', scenario: 'short-map-20000', op: 'events' },
    { label: 'Events: 4 KiB boundary', scenario: 'boundary-4kb-below', op: 'events' },
    { label: 'Events: 1 MiB scalar', scenario: 'large-scalar-1mb', op: 'events' },
    { label: 'Events: CI workflow', scenario: 'ci-workflow', op: 'events' },
    { label: 'Events: OpenAPI spec', scenario: 'openapi-spec', op: 'events' },
    { label: 'Borrowed decode: 4 KiB', scenario: 'boundary-4kb-below', op: 'borrowed' },
    { label: 'Value DOM: 1 MiB scalar', scenario: 'large-scalar-1mb', op: 'value' },
  ];
  return picks
    .map((p) => {
      const oldRow = v023SpeedMap.get(`${p.op}|${p.scenario}`);
      const curRow = getSpeed('ayaml', p.op, p.scenario);
      if (!oldRow || !curRow) {
        throw new Error(`bench.ts: missing v0.2.3 progress row for ${p.op}/${p.scenario}`);
      }
      const oldMem = v023MemoryMap.get(`${p.op}|${p.scenario}`);
      const curMem = getMemory('ayaml', p.op, p.scenario);
      return {
        ...p,
        v023Mib: oldRow.throughput_mib_s,
        curMib: curRow.throughput_mib_s,
        speedup: oldRow.median_ns / curRow.median_ns,
        peakRatio:
          oldMem && curMem && curMem.peak_bytes > 0
            ? oldMem.peak_bytes / curMem.peak_bytes
            : null,
      };
    })
    .sort((a, b) => b.speedup - a.speedup);
}

export interface ScalingStep {
  family: string;
  from: string;
  to: string;
  sizeRatio: number;
  timeNorm: number | null;
  peakNorm: number;
}

export function getScalingSteps(): ScalingStep[] {
  const pairs = [
    { family: 'Block mapping', from: 'scale-map-10kb', to: 'scale-map-100kb' },
    { family: 'Block mapping (memory)', from: 'scale-map-100kb', to: 'scale-map-1mb' },
    { family: 'Block sequence', from: 'scale-seq-10kb', to: 'scale-seq-100kb' },
    { family: 'Block sequence (memory)', from: 'scale-seq-100kb', to: 'scale-seq-1mb' },
    { family: 'Nested OpenAPI', from: 'scale-openapi-10kb', to: 'scale-openapi-100kb' },
    { family: 'Nested OpenAPI (memory)', from: 'scale-openapi-100kb', to: 'scale-openapi-1mb' },
  ];
  return pairs.map((p) => {
    const mA = mustMemory('ayaml', 'value', p.from);
    const mB = mustMemory('ayaml', 'value', p.to);
    const sA = getSpeed('ayaml', 'value', p.from);
    const sB = getSpeed('ayaml', 'value', p.to);
    const sizeRatio = mB.bytes / mA.bytes;
    return {
      family: p.family,
      from: p.from,
      to: p.to,
      sizeRatio,
      timeNorm: sA && sB ? sB.median_ns / sA.median_ns / sizeRatio : null,
      peakNorm: mB.peak_bytes / mA.peak_bytes / sizeRatio,
    };
  });
}

export function fmtBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KiB`;
  return `${(bytes / (1024 * 1024)).toFixed(2)} MiB`;
}

export function fmtTimeUs(us: number): string {
  if (us < 1000) return `${us.toFixed(1)} µs`;
  return `${(us / 1000).toFixed(2)} ms`;
}

// Headline stats used across homepage, benchmarks page, and llms*.txt — 100% computed from JSONL.
export const HEADLINE = (() => {
  const largeScalar = mustSpeed('ayaml', 'events', 'large-scalar-1mb');
  const largeScalarRyml = mustSpeed('rapidyaml', 'parse-arena', 'large-scalar-1mb');
  const largeScalarLibyaml = mustSpeed('libyaml', 'events', 'large-scalar-1mb');
  const ciEmit = mustSpeed('ayaml', 'emit', 'ci-workflow');
  const ciEmitYamlSerde = mustSpeed('yaml-serde', 'emit', 'ci-workflow');

  const mustPeer = (list: PeerComparisonRow[], impl: string, op?: string): PeerComparisonRow => {
    const found = list.find((r) => r.impl === impl && (op === undefined || r.op === op));
    if (!found) throw new Error(`bench.ts: missing peer ${impl}/${op ?? '*'}`);
    return found;
  };

  const vsYamlRust2 = mustPeer(TIER2_PEERS, 'yaml-rust2');
  const vsSaphyr = mustPeer(TIER2_PEERS, 'saphyr');
  const vsYamlSerde = mustPeer(TIER2_PEERS, 'yaml-serde');
  const vsSerdeSaphyr = mustPeer(TIER2_PEERS, 'serde-saphyr');
  const vsFkyaml = mustPeer(TIER2_PEERS, 'fkyaml');
  const vsLibyaml = mustPeer(TIER1_PEERS, 'libyaml', 'events');
  const vsLibfyaml = mustPeer(TIER1_PEERS, 'libfyaml', 'events');
  const vsRymlEvents = mustPeer(TIER1_PEERS, 'rapidyaml', 'parse-arena');
  const vsRymlValue = mustPeer(TIER2_PEERS, 'rapidyaml', 'resolve-typed');

  if (vsYamlSerde.allocRatio === null || vsYamlSerde.peakRatio === null) {
    throw new Error('bench.ts: missing yaml-serde memory ratios');
  }
  if (vsLibyaml.allocRatio === null) {
    throw new Error('bench.ts: missing libyaml events alloc ratio');
  }

  const emitMibs = EMIT_SCENARIOS.map((sc) => mustSpeed('ayaml', 'emit', sc).throughput_mib_s);
  const emitVs = (peerImpl: string) =>
    EMIT_SCENARIOS.map(
      (sc) => mustSpeed(peerImpl, 'emit', sc).median_ns / mustSpeed('ayaml', 'emit', sc).median_ns,
    );
  const emitFk = emitVs('fkyaml');
  const emitNoya = emitVs('noyalib');
  const emitYamlSerde = emitVs('yaml-serde');

  const typedPeakSavings = TYPED_SCENARIOS.map((sc) => {
    const ay = mustMemory('ayaml', 'borrowed', sc.id).peak_bytes;
    const ys = mustMemory('yaml-serde', 'typed', sc.id).peak_bytes;
    return ys / ay;
  });

  // Compute overall v0.2.3 -> current geomean across every shared speed & memory row.
  const v023SpeedRatios: number[] = [];
  for (const oldRow of v023SpeedRows) {
    const curRow = getSpeed('ayaml', oldRow.operation, oldRow.scenario);
    if (curRow && curRow.median_ns > 0 && oldRow.median_ns > 0) {
      v023SpeedRatios.push(oldRow.median_ns / curRow.median_ns);
    }
  }
  const v023PeakRatios: number[] = [];
  for (const oldRow of v023MemoryRows) {
    const curRow = getMemory('ayaml', oldRow.operation, oldRow.scenario);
    if (curRow && curRow.peak_bytes > 0 && oldRow.peak_bytes > 0) {
      v023PeakRatios.push(oldRow.peak_bytes / curRow.peak_bytes);
    }
  }

  if (v023SpeedRatios.length === 0) {
    throw new Error('bench.ts: no shared v0.2.3 speed rows for overall speedup');
  }
  if (v023PeakRatios.length === 0) {
    throw new Error('bench.ts: no shared v0.2.3 memory rows for peak improvement');
  }

  return {
    largeScalarMib: largeScalar.throughput_mib_s,
    largeScalarVsRyml: largeScalarRyml.median_ns / largeScalar.median_ns,
    largeScalarVsLibyaml: largeScalarLibyaml.median_ns / largeScalar.median_ns,
    ciEmitMib: ciEmit.throughput_mib_s,
    ciEmitVsYamlSerde: ciEmitYamlSerde.median_ns / ciEmit.median_ns,
    emitMinMib: Math.min(...emitMibs),
    emitMaxMib: Math.max(...emitMibs),
    emitVsFkyamlMin: Math.min(...emitFk),
    emitVsFkyamlMax: Math.max(...emitFk),
    emitVsNoyalibMin: Math.min(...emitNoya),
    emitVsNoyalibMax: Math.max(...emitNoya),
    emitVsYamlSerdeMin: Math.min(...emitYamlSerde),
    emitVsYamlSerdeMax: Math.max(...emitYamlSerde),
    typedMaxPeakSavingsVsYamlSerde: Math.max(...typedPeakSavings),
    vsYamlRust2: vsYamlRust2.speedRatio,
    vsSaphyr: vsSaphyr.speedRatio,
    vsYamlSerde: vsYamlSerde.speedRatio,
    vsYamlSerdeAllocs: vsYamlSerde.allocRatio,
    vsYamlSerdePeak: vsYamlSerde.peakRatio,
    vsSerdeSaphyr: vsSerdeSaphyr.speedRatio,
    vsLibyamlEvents: vsLibyaml.speedRatio,
    vsLibyamlEventsAllocs: vsLibyaml.allocRatio,
    vsLibfyamlEvents: vsLibfyaml.speedRatio,
    vsFkyaml: vsFkyaml.speedRatio,
    rymlAheadOfValue: 1 / vsRymlValue.speedRatio,
    rymlAheadOfEvents: 1 / vsRymlEvents.speedRatio,
    v023OverallSpeedup: geomean(v023SpeedRatios),
    v023SpeedRowCount: v023SpeedRatios.length,
    v023MinSpeedup: Math.min(...v023SpeedRatios),
    v023MaxSpeedup: Math.max(...v023SpeedRatios),
    v023PeakImprovement: geomean(v023PeakRatios),
    v023MemoryRowCount: v023PeakRatios.length,
    scenarioCount: ALL_SCENARIOS.length,
    memoryScenarioCount: memoryScenarioSet.size,
  };
})();
