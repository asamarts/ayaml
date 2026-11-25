import { readFileSync } from 'node:fs';
import { join, resolve } from 'node:path';
import { HEADLINE, fingerprint } from '../lib/bench';
import { DOCS_RS_AYAML, DOCS_RS_EDIT, GITHUB_URL, SITE_URL, WORKSPACE_VERSION } from '../lib/site';

export const prerender = true;

const REPO_ROOT = resolve(process.cwd(), '..');

function summaryLinks(): string[] {
  const summary = readFileSync(join(REPO_ROOT, 'docs', 'SUMMARY.md'), 'utf8');
  const out: string[] = [];
  for (const line of summary.split('\n')) {
    const m = /\[([^\]]+)\]\(([^)#]+\.md)\)/.exec(line);
    if (!m) continue;
    const label = m[1].trim();
    const rel = m[2].trim();
    const slug = rel === 'index.md' ? '' : `${rel.replace(/\.md$/, '')}/`;
    out.push(`- [${label}](${SITE_URL}/docs/${slug})`);
  }
  return out;
}

export const GET = async () => {
  const body = `# ayaml

> Safe, fast, pure-Rust YAML 1.2.2 parser, spanned event stream, generic value model, Serde codec, deterministic emitter, and format-preserving editor.

- Crate version: \`ayaml ${WORKSPACE_VERSION}\` (companion crate: \`ayaml-edit ${WORKSPACE_VERSION}\`)
- MSRV: Rust 1.88
- Safety: \`#![forbid(unsafe_code)]\` across published workspace crates
- Direct runtime dependencies: \`serde\`, \`indexmap\`
- Repository: ${GITHUB_URL}
- Rustdoc API: ${DOCS_RS_AYAML} and ${DOCS_RS_EDIT}

## Performance summary (pinned dataset ${fingerprint.captured_date})

- Geometric mean across ${HEADLINE.scenarioCount} \`ayaml-fixtures\` scenarios:
  - \`ayaml value\` is ${HEADLINE.vsYamlRust2.toFixed(2)}x faster than \`yaml-rust2\`, ${HEADLINE.vsSaphyr.toFixed(2)}x faster than \`saphyr\`, ${HEADLINE.vsYamlSerde.toFixed(2)}x faster than \`yaml-serde\` (${HEADLINE.vsYamlSerdeAllocs.toFixed(1)}x fewer allocations, ${HEADLINE.vsYamlSerdePeak.toFixed(2)}x lower peak heap), ${HEADLINE.vsSerdeSaphyr.toFixed(2)}x faster than \`serde-saphyr\`, and ${HEADLINE.vsFkyaml.toFixed(2)}x faster than C++ \`fkYAML\`.
  - \`ayaml events\` is ${HEADLINE.vsLibyamlEvents.toFixed(2)}x faster than C \`libyaml\` events and ${HEADLINE.vsLibfyamlEvents.toFixed(2)}x faster than C \`libfyaml\` events.
  - Large-scalar streaming reaches ${HEADLINE.largeScalarMib.toFixed(1)} MiB/s (${HEADLINE.largeScalarVsRyml.toFixed(2)}x faster than \`rapidyaml\`); deterministic emission reaches ${HEADLINE.ciEmitMib.toFixed(1)} MiB/s.
- Interactive showcase: ${SITE_URL}/benchmarks/

## Documentation pages

${summaryLinks().join('\n')}

## Full text bundle

- [llms-full.txt](${SITE_URL}/llms-full.txt)
`;

  return new Response(body, {
    headers: { 'Content-Type': 'text/plain; charset=utf-8' },
  });
};
