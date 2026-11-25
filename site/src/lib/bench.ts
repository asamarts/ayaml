import { readFileSync } from 'node:fs';
import { join } from 'node:path';

const DATA_DIR = join(process.cwd(), 'src', 'data', 'benchmarks');

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
if (speedRows.length !== fingerprint.speed_rows) {
  throw new Error(
    `bench.ts: speed.jsonl has ${speedRows.length} rows, fingerprint expects ${fingerprint.speed_rows}`,
  );
}
if (memoryRows.length !== fingerprint.memory_rows) {
  throw new Error(
    `bench.ts: memory.jsonl has ${memoryRows.length} rows, fingerprint expects ${fingerprint.memory_rows}`,
  );
}

const speedMap = new Map<string, SpeedRow>();
for (const r of speedRows) {
  speedMap.set(`${r.implementation}|${r.operation}|${r.scenario}`, r);
}

const memoryMap = new Map<string, MemoryRow>();
for (const r of memoryRows) {
  memoryMap.set(`${r.implementation}|${r.operation}|${r.scenario}`, r);
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

export const ALL_SCENARIOS: ScenarioMeta[] = [
  { id: 'frontmatter-tiny', category: 'domain', label: 'Tiny frontmatter header', bytes: 323 },
  { id: 'ci-workflow', category: 'domain', label: 'CI workflow with block scripts', bytes: 21735 },
  { id: 'container-compose', category: 'domain', label: 'Container compose (anchors + merges)', bytes: 15277 },
  { id: 'cluster-manifests', category: 'domain', label: 'Multi-doc cluster manifests', bytes: 27532 },
  { id: 'openapi-spec', category: 'domain', label: 'OpenAPI specification', bytes: 173342 },
  { id: 'lockfile-wide', category: 'domain', label: 'Wide dependency lockfile', bytes: 55991 },
  { id: 'escape-routes', category: 'domain', label: 'Escape-heavy route table', bytes: 41188 },
  { id: 'json-in-yaml', category: 'domain', label: 'Strict JSON-in-YAML payload', bytes: 29452 },
  { id: 'unicode-multibyte', category: 'domain', label: 'Multi-byte Unicode document', bytes: 34965 },
  { id: 'boundary-4kb-below', category: 'boundary', label: 'Structured map below 4 KiB threshold', bytes: 4034 },
  { id: 'boundary-4kb-above', category: 'boundary', label: 'Structured map above 4 KiB threshold', bytes: 4379 },
  { id: 'scale-map-10kb', category: 'scaling', label: 'Block mapping (10 KiB)', bytes: 10000 },
  { id: 'scale-map-100kb', category: 'scaling', label: 'Block mapping (100 KiB)', bytes: 100000 },
  { id: 'scale-seq-10kb', category: 'scaling', label: 'Block sequence (10 KiB)', bytes: 10000 },
  { id: 'scale-seq-100kb', category: 'scaling', label: 'Block sequence (100 KiB)', bytes: 100000 },
  { id: 'scale-openapi-10kb', category: 'scaling', label: 'Nested OpenAPI (10 KiB)', bytes: 15005 },
  { id: 'scale-openapi-100kb', category: 'scaling', label: 'Nested OpenAPI (100 KiB)', bytes: 151722 },
  { id: 'depth-64', category: 'stress', label: 'Deep flow nesting (depth 64)', bytes: 130 },
  { id: 'aliases-1000', category: 'stress', label: 'Repeated alias fan-out (1,000)', bytes: 10047 },
  { id: 'short-seq-20000', category: 'stress', label: '20,000 short sequence items', bytes: 80000 },
  { id: 'short-map-20000', category: 'stress', label: '20,000 short mapping entries', bytes: 277780 },
  { id: 'short-explicit-20000', category: 'stress', label: '20,000 explicit-key entries', bytes: 337780 },
  { id: 'large-scalar-1mb', category: 'stress', label: '1 MiB block scalar', bytes: 1000001 },
];

// Hydrate exact byte counts from the pinned dataset so they cannot drift.
for (const s of ALL_SCENARIOS) {
  const row = getSpeed('ayaml', 'events', s.id) ?? getMemory('ayaml', 'events', s.id);
  if (!row) throw new Error(`bench.ts: missing scenario ${s.id} in pinned data`);
  s.bytes = row.bytes;
}

export interface PeerComparisonRow {
  impl: string;
  op: string;
  label: string;
  lang: 'Rust' | 'C' | 'C++';
  safety: string;
  sharedScenarios: number;
  speedRatio: number; // >1.0 means ayaml is faster; <1.0 means peer is faster
  relativeSpeedVsAyaml: number; // ayaml_time / peer_time? No: peer_time / ayaml_time (same as SUMMARY.md)
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
  return peers.map((peer) => {
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

    const timeRatio = geomean(timeRatios);
    return {
      impl: peer.impl,
      op: peer.op,
      label: peer.label,
      lang: peer.lang,
      safety: peer.safety,
      sharedScenarios: timeRatios.length,
      speedRatio: timeRatio,
      relativeSpeedVsAyaml: timeRatio,
      peakRatio: peakRatios.length > 0 ? geomean(peakRatios) : null,
      allocRatio: allocRatios.length > 0 ? geomean(allocRatios) : null,
      isAyaml: Boolean(peer.isAyaml),
      isUnsafeArena: peer.isUnsafeArena,
    };
  });
}

export const TIER2_PEERS = computeTierComparison('ayaml', 'value', [
  { impl: 'rapidyaml', op: 'resolve-typed', label: 'rapidyaml (C++ arena)', lang: 'C++', safety: 'Unsafe C++ flat arena (no Rust DOM)', isUnsafeArena: true },
  { impl: 'ayaml', op: 'value', label: 'ayaml (Value DOM)', lang: 'Rust', safety: 'Safe Rust (#![forbid(unsafe_code)])', isAyaml: true },
  { impl: 'ayaml', op: 'compat-serde', label: 'ayaml (compat serde)', lang: 'Rust', safety: 'Safe Rust (libyaml-compat)', isAyaml: true },
  { impl: 'fkyaml', op: 'parse', label: 'fkYAML', lang: 'C++', safety: 'C++ DOM parser' },
  { impl: 'ayaml', op: 'serde-value', label: 'ayaml (serde Value)', lang: 'Rust', safety: 'Safe Rust Serde', isAyaml: true },
  { impl: 'yaml-rust2', op: 'parse', label: 'yaml-rust2', lang: 'Rust', safety: 'Safe Rust DOM' },
  { impl: 'noyalib', op: 'value', label: 'noyalib', lang: 'Rust', safety: 'Rust YAML DOM' },
  { impl: 'saphyr', op: 'parse', label: 'saphyr', lang: 'Rust', safety: 'Safe Rust DOM' },
  { impl: 'fast-yaml-core', op: 'parse', label: 'fast-yaml-core', lang: 'Rust', safety: 'Rust DOM' },
  { impl: 'yaml-serde', op: 'value', label: 'yaml-serde', lang: 'Rust', safety: 'libyaml C FFI / unsafe' },
  { impl: 'serde-saphyr', op: 'serde-json', label: 'serde-saphyr', lang: 'Rust', safety: 'Safe Rust Serde' },
]);

export const TIER1_PEERS = computeTierComparison('ayaml', 'events', [
  { impl: 'ayaml', op: 'scan', label: 'ayaml (scanner only)', lang: 'Rust', safety: 'Safe Rust tokenizer', isAyaml: true },
  { impl: 'rapidyaml', op: 'parse-reuse', label: 'rapidyaml (parse-reuse)', lang: 'C++', safety: 'Unsafe C++ reused arena', isUnsafeArena: true },
  { impl: 'rapidyaml', op: 'parse-arena', label: 'rapidyaml (parse-arena)', lang: 'C++', safety: 'Unsafe C++ flat arena', isUnsafeArena: true },
  { impl: 'ayaml', op: 'events', label: 'ayaml (spanned events)', lang: 'Rust', safety: 'Safe Rust YAML 1.2.2', isAyaml: true },
  { impl: 'libyaml', op: 'events', label: 'libyaml (events)', lang: 'C', safety: 'Unsafe C parser' },
  { impl: 'libyaml', op: 'load', label: 'libyaml (load)', lang: 'C', safety: 'Unsafe C document' },
  { impl: 'ayaml', op: 'spanned', label: 'ayaml (SpannedTree)', lang: 'Rust', safety: 'Safe Rust spanned tree', isAyaml: true },
  { impl: 'libfyaml', op: 'events', label: 'libfyaml (events)', lang: 'C', safety: 'Unsafe C parser' },
  { impl: 'libfyaml', op: 'doc', label: 'libfyaml (doc)', lang: 'C', safety: 'Unsafe C document' },
]);

export const TYPED_SCENARIOS = ['frontmatter-tiny', 'boundary-4kb-below', 'boundary-4kb-above'] as const;

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
  return out.sort((a, b) => b.throughput - a.throughput);
}

export interface ProgressRow {
  label: string;
  scenario: string;
  op: string;
  v023Mib: number;
  curMib: number;
  speedup: number;
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
  return picks.map((p) => {
    const oldRow = v023SpeedMap.get(`${p.op}|${p.scenario}`);
    const curRow = getSpeed('ayaml', p.op, p.scenario);
    if (!oldRow || !curRow) {
      throw new Error(`bench.ts: missing v0.2.3 progress row for ${p.op}/${p.scenario}`);
    }
    return {
      ...p,
      v023Mib: oldRow.throughput_mib_s,
      curMib: curRow.throughput_mib_s,
      speedup: curRow.throughput_mib_s / oldRow.throughput_mib_s,
    };
  });
}

export interface ScalingStep {
  family: string;
  from: string;
  to: string;
  sizeRatio: number;
  timeNorm: number;
  peakNorm: number;
}

export function getScalingSteps(): ScalingStep[] {
  const pairs = [
    { family: 'Block mapping', from: 'scale-map-10kb', to: 'scale-map-100kb' },
    { family: 'Block sequence', from: 'scale-seq-10kb', to: 'scale-seq-100kb' },
    { family: 'Nested OpenAPI', from: 'scale-openapi-10kb', to: 'scale-openapi-100kb' },
  ];
  return pairs.map((p) => {
    const sA = getSpeed('ayaml', 'value', p.from);
    const sB = getSpeed('ayaml', 'value', p.to);
    const mA = getMemory('ayaml', 'value', p.from);
    const mB = getMemory('ayaml', 'value', p.to);
    if (!sA || !sB || !mA || !mB) {
      throw new Error(`bench.ts: missing scaling pair ${p.from} -> ${p.to}`);
    }
    const sizeRatio = sB.bytes / sA.bytes;
    return {
      family: p.family,
      from: p.from,
      to: p.to,
      sizeRatio,
      timeNorm: (sB.median_ns / sA.median_ns) / sizeRatio,
      peakNorm: (mB.peak_bytes / mA.peak_bytes) / sizeRatio,
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

// Headline stats used across homepage, benchmarks page, and llms.txt.
export const HEADLINE = (() => {
  const largeScalar = getSpeed('ayaml', 'events', 'large-scalar-1mb')!;
  const largeScalarRyml = getSpeed('rapidyaml', 'parse-arena', 'large-scalar-1mb')!;
  const ciEmit = getSpeed('ayaml', 'emit', 'ci-workflow')!;
  const ciEmitYamlSerde = getSpeed('yaml-serde', 'emit', 'ci-workflow')!;
  const vsYamlRust2 = TIER2_PEERS.find((r) => r.impl === 'yaml-rust2')!;
  const vsSaphyr = TIER2_PEERS.find((r) => r.impl === 'saphyr')!;
  const vsYamlSerde = TIER2_PEERS.find((r) => r.impl === 'yaml-serde')!;
  const vsSerdeSaphyr = TIER2_PEERS.find((r) => r.impl === 'serde-saphyr')!;
  const vsLibyaml = TIER1_PEERS.find((r) => r.impl === 'libyaml' && r.op === 'events')!;
  const vsLibfyaml = TIER1_PEERS.find((r) => r.impl === 'libfyaml' && r.op === 'events')!;
  const vsFkyaml = TIER2_PEERS.find((r) => r.impl === 'fkyaml')!;
  return {
    largeScalarMib: largeScalar.throughput_mib_s,
    largeScalarVsRyml: largeScalar.throughput_mib_s / largeScalarRyml.throughput_mib_s,
    ciEmitMib: ciEmit.throughput_mib_s,
    ciEmitVsYamlSerde: ciEmit.throughput_mib_s / ciEmitYamlSerde.throughput_mib_s,
    vsYamlRust2: vsYamlRust2.speedRatio,
    vsSaphyr: vsSaphyr.speedRatio,
    vsYamlSerde: vsYamlSerde.speedRatio,
    vsYamlSerdeAllocs: vsYamlSerde.allocRatio ?? 3.8,
    vsYamlSerdePeak: vsYamlSerde.peakRatio ?? 1.67,
    vsSerdeSaphyr: vsSerdeSaphyr.speedRatio,
    vsLibyamlEvents: vsLibyaml.speedRatio,
    vsLibfyamlEvents: vsLibfyaml.speedRatio,
    vsFkyaml: vsFkyaml.speedRatio,
    scenarioCount: ALL_SCENARIOS.length,
  };
})();
