import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { HEADLINE, fingerprint } from '../lib/bench';
import {
  DOCS_RS_AYAML,
  DOCS_RS_EDIT,
  GITHUB_URL,
  REPO_ROOT,
  SITE_URL,
  WORKSPACE_VERSION,
} from '../lib/site';
import {
  extractDescription,
  extractTitleAndBody,
  relMdToDocsSubpath,
} from '../../scripts/markdown-utils.mjs';

export const prerender = true;

function groupedSummaryMarkdown(): string {
  const summary = readFileSync(join(REPO_ROOT, 'docs', 'SUMMARY.md'), 'utf8');
  const lines: string[] = [];
  for (const rawLine of summary.split('\n')) {
    const trimmed = rawLine.trim();
    if (!trimmed || trimmed.startsWith('<!--')) continue;
    const heading = /^#\s+(.+)$/.exec(trimmed);
    if (heading) {
      const label = heading[1].trim();
      if (label.toLowerCase() !== 'summary') {
        lines.push('', `### ${label}`, '');
      }
      continue;
    }
    const m = /\[([^\]]+)\]\(([^)#]+\.md)\)/.exec(trimmed);
    if (!m) continue;
    const label = m[1].trim();
    const rel = m[2].trim().replace(/^\.\//, '');
    const subpath = relMdToDocsSubpath(rel);
    const content = readFileSync(join(REPO_ROOT, 'docs', rel), 'utf8');
    const { title, body } = extractTitleAndBody(content, label);
    const desc = extractDescription(body, title);
    lines.push(`- [${label}](${SITE_URL}/docs/${subpath}): ${desc}`);
  }
  return lines.join('\n').trim();
}

export const GET = async () => {
  const body = `# ayaml

> Pure-Rust YAML 1.2.2 parser, Serde codec, spanned event stream, deterministic emitter, and source-preserving editor.

- Crate version: \`ayaml ${WORKSPACE_VERSION}\` (companion crate: \`ayaml-edit ${WORKSPACE_VERSION}\`)
- MSRV: Rust 1.88
- Safety: \`#![forbid(unsafe_code)]\` across published workspace crates
- Direct runtime dependencies: \`serde\`, \`indexmap\`
- Repository: ${GITHUB_URL}
- Rustdoc API: ${DOCS_RS_AYAML} and ${DOCS_RS_EDIT}

## Performance summary (pinned dataset ${fingerprint.captured_date})

- Geometric mean across shared \`ayaml-fixtures\` workloads (${HEADLINE.scenarioCount} timed fixtures, ${HEADLINE.memoryScenarioCount} memory fixtures):
  - \`ayaml value\` is ${HEADLINE.vsYamlRust2.toFixed(2)}x faster than \`yaml-rust2\`, ${HEADLINE.vsSaphyr.toFixed(2)}x faster than \`saphyr\`, ${HEADLINE.vsYamlSerde.toFixed(2)}x faster than \`yaml-serde\` (${HEADLINE.vsYamlSerdeAllocs.toFixed(1)}x fewer allocations, ${HEADLINE.vsYamlSerdePeak.toFixed(2)}x lower peak heap), ${HEADLINE.vsSerdeSaphyr.toFixed(2)}x faster than \`serde-saphyr\`, and ${HEADLINE.vsFkyaml.toFixed(2)}x faster than C++ \`fkYAML\`.
  - \`ayaml events\` is ${HEADLINE.vsLibyamlEvents.toFixed(2)}x faster than C \`libyaml\` events and ${HEADLINE.vsLibfyamlEvents.toFixed(2)}x faster than C \`libfyaml\` events.
  - Large-scalar streaming reaches ${HEADLINE.largeScalarMib.toFixed(1)} MiB/s (${HEADLINE.largeScalarVsRyml.toFixed(2)}x faster than \`rapidyaml\`); deterministic emission reaches ${HEADLINE.ciEmitMib.toFixed(1)} MiB/s.
- Benchmarks: ${SITE_URL}/benchmarks/

## Documentation pages

${groupedSummaryMarkdown()}

## Full text bundle

- [llms-full.txt](${SITE_URL}/llms-full.txt)
`;

  return new Response(body, {
    headers: { 'Content-Type': 'text/plain; charset=utf-8' },
  });
};
