//! Realistic YAML 1.2.2 domain workload generators with fixed seeds.

use std::fmt::Write as _;

use crate::prng::Rng;

/// Multi-document cluster Deployment, Service, and CRD manifests with nested `OpenAPI` schemas.
pub(crate) fn cluster_manifests(seed: u64, workloads: usize) -> String {
    let mut rng = Rng::new(seed);
    let mut out = String::with_capacity(workloads * 1_600);
    for idx in 0..workloads {
        let svc = format!("{}-{idx:03}", rng.word());
        let env = rng.env_name();
        let replicas = 2 + rng.below(6);
        let port = 8000 + (idx % 100);
        let _ = writeln!(
            out,
            "\
---
apiVersion: apps/v1
kind: Deployment
metadata:
  name: {svc}
  namespace: {env}
  labels:
    app.kubernetes.io/name: {svc}
    app.kubernetes.io/tier: backend
    release: v{}
spec:
  replicas: {replicas}
  selector:
    matchLabels:
      app: {svc}
  template:
    metadata:
      labels:
        app: {svc}
        env: {env}
    spec:
      containers:
        - name: app
          image: registry.example.internal/{svc}:{}
          ports:
            - containerPort: {port}
              protocol: TCP
          env:
            - name: SERVICE_ENV
              value: {env}
            - name: WORKER_THREADS
              value: \"{}\"
            - name: TRACE_SAMPLE_RATE
              value: \"0.{:02}\"
          resources:
            requests:
              cpu: \"250m\"
              memory: \"256Mi\"
            limits:
              cpu: \"1000m\"
              memory: \"1Gi\"
---
apiVersion: v1
kind: Service
metadata:
  name: {svc}-svc
  namespace: {env}
spec:
  type: ClusterIP
  selector:
    app: {svc}
  ports:
    - name: http
      port: 80
      targetPort: {port}
",
            rng.semver(),
            rng.semver(),
            2 + rng.below(8),
            5 + rng.below(90),
        );
    }
    out.push_str(
        "\
---
apiVersion: apiextensions.k8s.io/v1
kind: CustomResourceDefinition
metadata:
  name: pipelines.ops.example.io
spec:
  group: ops.example.io
  names:
    kind: Pipeline
    plural: pipelines
    singular: pipeline
  scope: Namespaced
  versions:
    - name: v1alpha1
      served: true
      storage: true
      schema:
        openAPIV3Schema:
          type: object
          required: [spec]
          properties:
            spec:
              type: object
              properties:
                concurrency:
                  type: integer
                  minimum: 1
                  maximum: 64
                retries:
                  type: integer
                  default: 3
                tags:
                  type: array
                  items:
                    type: string
",
    );
    out
}

/// Large `OpenAPI` 3.1 service specification with nested paths, parameters, flow schemas, and refs.
pub(crate) fn openapi_spec(seed: u64, path_count: usize) -> String {
    let mut rng = Rng::new(seed);
    let mut out = String::with_capacity(path_count * 980 + 512);
    out.push_str(
        "\
openapi: 3.1.0
info:
  title: Synthetic Platform Service API
  version: 2.4.0
  summary: Deterministic OpenAPI specification for parser and emitter measurements
  description: |
    Multi-line block description covering pagination, idempotency keys,
    structured rate-limit headers, and schema evolution guarantees.
servers:
  - url: https://api.example.internal/v2
    description: Primary regional endpoint
paths:
",
    );
    for i in 0..path_count {
        let resource = rng.word();
        let _ = writeln!(out, "  /v2/{resource}/{i}:");
        for verb in ["get", "post"] {
            let tag_a = rng.word();
            let tag_b = rng.word();
            let s1 = rng.word();
            let s2 = rng.word();
            let s3 = rng.word();
            let _ = writeln!(
                out,
                "    {verb}:\n      operationId: {verb}_{resource}_{i}\n      summary: \"{s1} {s2} {s3} operation\"\n      tags: [{tag_a}, {tag_b}]\n      parameters:"
            );
            for p in 0..3 {
                let max_len = 16 + rng.below(128);
                let required = if p == 0 { "true" } else { "false" };
                let _ = writeln!(
                    out,
                    "        - name: param_{p}\n          in: query\n          required: {required}\n          schema: {{type: string, minLength: 1, maxLength: {max_len}}}"
                );
            }
            out.push_str(
                "      responses:\n        '200':\n          description: ok\n          content:\n            application/json:\n              schema:\n                $ref: '#/components/schemas/Envelope'\n",
            );
        }
    }
    out.push_str(
        "\
components:
  schemas:
    Envelope:
      type: object
      required: [id, status]
      properties:
        id:
          type: string
        status:
          type: [string, 'null']
        latency_ms:
          type: number
",
    );
    out
}

/// CI workflow with `on:` mapping key, matrix strategies, and literal/folded shell script blocks.
pub(crate) fn ci_workflow(seed: u64, jobs: usize) -> String {
    let mut rng = Rng::new(seed);
    let mut out = String::with_capacity(jobs * 1_100 + 512);
    out.push_str(
        "\
name: continuous-verification
on:
  push:
    branches: [main]
  pull_request:
    paths-ignore:
      - \"docs/**/*.png\"
  schedule:
    - cron: \"17 4 * * *\"
permissions:
  contents: read
env:
  CARGO_TERM_COLOR: always
  RUST_BACKTRACE: \"1\"
jobs:
",
    );
    for idx in 0..jobs {
        let name = format!("{}_{idx:02}", rng.word());
        let env = rng.env_name();
        let timeout = 15 + rng.below(30);
        let seed_tok = rng.hex_token(8);
        let _ = write!(
            out,
            "  job_{name}:
    name: verify {name} ({env})
    runs-on: ubuntu-24.04
    timeout-minutes: {timeout}
    strategy:
      fail-fast: false
      matrix:
        toolchain: [\"1.88.0\", stable]
        profile: [debug, release]
    steps:
      - name: Checkout sources
        uses: actions/checkout@v4
        with:
          persist-credentials: false
      - name: Run build and checks
        env:
          TARGET_ENV: {env}
          JOB_SEED: \"{seed_tok}\"
        run: |
          set -euo pipefail
          echo \"starting {name} in $TARGET_ENV\"
          for round in 1 2 3; do
            printf 'round=%s seed=%s\\n' \"$round\" \"$JOB_SEED\"
          done
      - name: Summarize outcome
        description: >
          Collects timings, exit codes, and summary counters across
          the matrix run for {name} so regressions surface quickly.
        run: echo \"done {name}\"
"
        );
    }
    out
}

/// Container orchestration spec with `x-` extension anchors, aliases, and merge keys (`<<:`).
pub(crate) fn container_compose(seed: u64, services: usize) -> String {
    let mut rng = Rng::new(seed);
    let mut out = String::with_capacity(services * 600 + 768);
    out.push_str(
        "\
x-logging: &default_logging
  driver: json-file
  options:
    max-size: \"20m\"
    max-file: \"5\"

x-healthcheck: &default_healthcheck
  interval: 15s
  timeout: 5s
  retries: 3
  start_period: 10s

x-common-env: &common_env
  LOG_FORMAT: json
  METRICS_ENABLED: \"true\"
  REGION: us-east-1

x-service-base: &service_base
  restart: unless-stopped
  logging: *default_logging
  healthcheck: *default_healthcheck
  networks:
    - app_net
    - obs_net

services:
",
    );
    for idx in 0..services {
        let svc = format!("{}_{idx:03}", rng.word());
        let port = 9000 + idx;
        let mem = 128 + rng.below(512);
        let ver = rng.semver();
        let _ = write!(
            out,
            "  {svc}:
    <<: *service_base
    image: registry.example.internal/{svc}:{ver}
    ports:
      - \"{port}:{port}\"
    environment:
      <<: *common_env
      SERVICE_NAME: {svc}
      LISTEN_PORT: \"{port}\"
      CACHE_MB: \"{mem}\"
    deploy:
      resources:
        limits:
          memory: \"{mem}M\"
"
        );
    }
    out.push_str(
        "\
networks:
  app_net:
    driver: bridge
  obs_net:
    driver: bridge
",
    );
    out
}

/// One small (~900 B) automation playbook file, indexed deterministically for bulk small-file runs.
pub(crate) fn automation_playbook(seed: u64, index: usize) -> String {
    let mut rng = Rng::new(seed ^ ((index as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15)));
    let role = rng.word();
    let env = rng.env_name();
    let ver = rng.semver();
    format!(
        "\
---
- name: Configure {role} node {index:04}
  hosts: {role}_{env}
  become: true
  vars:
    service_name: {role}-{index:04}
    service_version: \"{ver}\"
    config_dir: \"/etc/{role}\"
    listen_port: {}
    enabled_flags: [metrics, tracing, graceful_shutdown]
  tasks:
    - name: Ensure configuration directory exists
      file:
        path: \"{{{{ config_dir }}}}\"
        state: directory
        mode: \"0755\"
    - name: Render service configuration template
      template:
        src: \"{role}.yml.j2\"
        dest: \"{{{{ config_dir }}}}/config.yml\"
        mode: \"0640\"
      notify:
        - reload {role}
    - name: Verify health endpoint responds
      uri:
        url: \"http://127.0.0.1:{{{{ listen_port }}}}/healthz\"
        status_code: 200
  handlers:
    - name: reload {role}
      service:
        name: \"{{{{ service_name }}}}\"
        state: reloaded
",
        8100 + (index % 500),
    )
}

/// Multi-document stream of small automation playbooks.
pub(crate) fn automation_bulk_stream(seed: u64, count: usize) -> String {
    let mut out = String::with_capacity(count * 900);
    for idx in 0..count {
        out.push_str(&automation_playbook(seed, idx));
    }
    out
}

/// Wide lockfile-style mapping with quoted scoped package keys, integrity digests, and empty maps.
pub(crate) fn lockfile_wide(seed: u64, packages: usize) -> String {
    let mut rng = Rng::new(seed);
    let mut out = String::with_capacity(packages * 260 + 256);
    out.push_str(
        "\
lockfileVersion: '9.0'
settings:
  autoInstallPeers: true
  excludeLinksFromLockfile: false
importers:
  .:
    dependencies:
      '@core/runtime':
        specifier: ^2.0.0
        version: 2.0.4
packages:
",
    );
    let mut keys = Vec::with_capacity(packages);
    for idx in 0..packages {
        let scope = rng.word();
        let pkg = format!("'@scope-{scope}/pkg-{idx:04}@{}'", rng.semver());
        let integrity = rng.hex_token(24);
        let _ = writeln!(
            out,
            "  {pkg}:\n    resolution: {{integrity: sha512-{integrity}}}\n    engines: {{node: '>=20.0.0'}}"
        );
        keys.push(pkg);
    }
    out.push_str("snapshots:\n");
    for (idx, pkg) in keys.iter().enumerate() {
        if idx % 3 == 0 {
            let _ = writeln!(out, "  {pkg}: {{}}");
        } else {
            let dep = &keys[(idx + 1) % keys.len()];
            let _ = writeln!(out, "  {pkg}:\n    dependencies:\n      '@dep/next': {dep}");
        }
    }
    out
}

/// Infrastructure and application config template heavy in local/custom tags.
pub(crate) fn custom_tags(seed: u64, resources: usize) -> String {
    let mut rng = Rng::new(seed);
    let mut out = String::with_capacity(resources * 480 + 256);
    out.push_str(
        "\
AWSTemplateFormatVersion: '2010-09-09'
Description: Deterministic tagged template fixture
AuthMode: !Disabled
Storage: !Postgres
  host: db.example.internal
  port: 5432
  ssl: true
Resources:
",
    );
    for idx in 0..resources {
        let name = format!("WorkerResource{idx:03}");
        let role = rng.word();
        let _ = write!(
            out,
            "  {name}:
    Type: AWS::Lambda::Function
    Properties:
      FunctionName: !Sub \"${{AWS::StackName}}-{role}-{idx:03}\"
      Role: !GetAtt ExecutionRole.Arn
      Runtime: provided.al2023
      Environment:
        Variables:
          BUCKET: !Ref ArtifactBucket
          ENDPOINT: !Join ['', ['https://', !Ref DomainName, '/{role}/{idx}']]
          FEATURE_FLAG: !If [IsProd, enabled, disabled]
          TAGS: !reference [.base_tags, items]
"
        );
    }
    out
}

/// Escape-heavy proxy route and regex config exercising double-quoted escape decoding.
pub(crate) fn escape_routes(seed: u64, routes: usize) -> String {
    let mut rng = Rng::new(seed);
    let mut out = String::with_capacity(routes * 360 + 256);
    out.push_str(
        "\
static_resources:
  listeners:
    - name: ingress_http
      address: \"0.0.0.0:8443\"
      routes:
",
    );
    for idx in 0..routes {
        let svc = rng.word();
        let token = rng.hex_token(6);
        let _ = write!(
            out,
            "        - name: \"route_{svc}_{idx:03}\"
          match:
            safe_regex: \"^/api/v[1-3]/{svc}/([a-z0-9_\\\\-]+)/(?:items|nodes)/{idx}$\"
            headers:
              - name: \"x-trace-pattern\"
                value: \"\\\"prefix:{token}\\\"\\t\\n\\u001b[32m{svc}\\u001b[0m\"
          route:
            cluster: \"{svc}_cluster\"
            timeout: \"15s\"
            response_headers_to_add:
              - header:
                  key: \"x-banner\"
                  value: \"line1\\nline2\\t\\\"quoted\\\"\\\\path\\\\{idx}\\u0021\"
"
        );
    }
    out
}

/// Tiny (~380-byte) YAML frontmatter/task metadata header targeting fixed-overhead latency.
pub(crate) fn frontmatter_tiny() -> String {
    "\
id: TASK-042
title: Validate parser spans and position conversions
phase: 3
status: in_progress
effort_days: 1.5
blocked_by:
  - TASK-010
  - TASK-021
paths:
  - crates/ayaml/src/span.rs
  - crates/ayaml/src/position.rs
tags:
  - correctness
  - spans
evidence:
  - kind: test
    ref: span_and_location_boundary_mutations
"
    .to_owned()
}

/// Strict RFC 8259 JSON payload parsed directly as YAML 1.2.2.
pub(crate) fn json_in_yaml(seed: u64, records: usize) -> String {
    let mut rng = Rng::new(seed);
    let mut out = String::with_capacity(records * 220 + 128);
    out.push_str("{\n  \"schema\": \"v1\",\n  \"count\": ");
    let _ = write!(out, "{records}");
    out.push_str(",\n  \"records\": [\n");
    for idx in 0..records {
        let comma = if idx + 1 == records { "" } else { "," };
        let kind = rng.word();
        let active = if idx % 2 == 0 { "true" } else { "false" };
        let tenths = idx % 997;
        let score_whole = tenths / 10;
        let score_frac = tenths % 10;
        let note = if idx % 5 == 0 {
            "null".to_owned()
        } else {
            format!("\"record \\\"#{idx}\\\"\\\\{kind}\"")
        };
        let _ = writeln!(
            out,
            "    {{\"id\": {idx}, \"kind\": \"{kind}\", \"active\": {active}, \"score\": {score_whole}.{score_frac}, \"tags\": [\"{}\", \"{}\"], \"note\": {note}}}{comma}",
            rng.env_name(),
            rng.word(),
        );
    }
    out.push_str("  ]\n}\n");
    out
}

/// Multi-byte UTF-8 document exercising 2-, 3-, and 4-byte UTF-8 characters and surrogate pairs.
pub(crate) fn unicode_multibyte(seed: u64, entries: usize) -> String {
    const PHRASES: [&str; 6] = [
        "café naïve résumé — über",
        "日本語のテスト設定と漢字",
        "Привет мир кириллица",
        "مرحبا بالعالم نص عربي",
        "🚀🌟🔥🎯🧪📦✨",
        "Ελληνικά & Tiếng Việt — €100",
    ];
    let mut rng = Rng::new(seed);
    let mut out = String::with_capacity(entries * 280 + 128);
    out.push_str("locale_catalog:\n  version: \"1.0-utf8\"\n  entries:\n");
    for idx in 0..entries {
        let p1 = PHRASES[idx % PHRASES.len()];
        let p2 = PHRASES[(idx + rng.below(PHRASES.len())) % PHRASES.len()];
        let tag = rng.word();
        let _ = write!(
            out,
            "    - id: \"clé_{idx:04}_🚀\"
      label: \"{p1}\"
      plain_text: {p2}
      summary: |
        {p1}
        {p2} (#{idx})
      tags: [\"é\", \"€\", \"🚀\", \"{tag}\"]
"
        );
    }
    out
}
