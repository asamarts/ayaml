import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { HEADLINE, fingerprint } from '../lib/bench';
import { REPO_ROOT, SITE_URL, WORKSPACE_VERSION } from '../lib/site';
import { relMdToDocsSubpath, transformDocsMarkdown } from '../../scripts/markdown-utils.mjs';

export const prerender = true;

export const GET = async () => {
  const summary = readFileSync(join(REPO_ROOT, 'docs', 'SUMMARY.md'), 'utf8');
  const sections: string[] = [
    [
      `# ayaml ${WORKSPACE_VERSION}: Complete Documentation`,
      '',
      `Canonical URL: ${SITE_URL}/`,
      `Benchmarks (${fingerprint.captured_date}, ${fingerprint.criterion?.rounds ?? 3} rounds): ${SITE_URL}/benchmarks/ (harness: ${fingerprint.harness_repo ?? 'https://github.com/asamarts/ayaml-benchmarks'})`,
      `- Geometric mean across shared fixtures (${HEADLINE.scenarioCount} timed, ${HEADLINE.memoryScenarioCount} memory): \`ayaml value\` is ${HEADLINE.vsYamlRust2.toFixed(2)}x faster than \`yaml-rust2\`, ${HEADLINE.vsSaphyr.toFixed(2)}x faster than \`saphyr\`, ${HEADLINE.vsYamlSerde.toFixed(2)}x faster than \`yaml-serde\` (${HEADLINE.vsYamlSerdeAllocs.toFixed(1)}x fewer allocations, ${HEADLINE.vsYamlSerdePeak.toFixed(2)}x lower peak heap), ${HEADLINE.vsSerdeSaphyr.toFixed(2)}x faster than \`serde-saphyr\` (targeting \`serde_json::Value\`), and ${HEADLINE.vsFkyaml.toFixed(2)}x faster than C++ \`fkYAML\`; public pull-based \`ayaml::parse_events\` is ${HEADLINE.vsLibyamlEvents.toFixed(2)}x faster than C \`libyaml\` events (${HEADLINE.vsLibyamlEventsExclLargeScalar.toFixed(2)}x excluding \`large-scalar-1mb\`, ${HEADLINE.vsLibyamlEventsPeak.toFixed(2)}x lower peak heap, ${HEADLINE.vsLibyamlEventsAllocs.toFixed(0)}x fewer allocations) and ${HEADLINE.vsLibfyamlEvents.toFixed(2)}x faster than C \`libfyaml\` events.`,
      `- On \`large-scalar-1mb\` (1 MiB plain scalar), \`ayaml::parse_events\` reaches ${HEADLINE.largeScalarMib.toFixed(1)} MiB/s (${HEADLINE.largeScalarVsLibfyaml.toFixed(2)}x vs C \`libfyaml\` events at ${HEADLINE.largeScalarLibfyamlMib.toFixed(0)} MiB/s and ${HEADLINE.largeScalarVsRyml.toFixed(2)}x vs C++ \`rapidyaml\`); deterministic emission reaches ${HEADLINE.ciEmitMib.toFixed(1)} MiB/s.`,
      '',
    ].join('\n'),
  ];

  for (const line of summary.split('\n')) {
    const m = /\[([^\]]+)\]\(([^)#]+\.md)\)/.exec(line);
    if (!m) continue;
    const rel = m[2].trim().replace(/^\.\//, '');
    const subpath = relMdToDocsSubpath(rel);
    const raw = readFileSync(join(REPO_ROOT, 'docs', rel), 'utf8').trim();
    const cleaned = transformDocsMarkdown(raw, rel, SITE_URL).trim();
    sections.push(`\n---\nURL: ${SITE_URL}/docs/${subpath}\n\n${cleaned}\n`);
  }

  return new Response(sections.join('\n'), {
    headers: { 'Content-Type': 'text/plain; charset=utf-8' },
  });
};
