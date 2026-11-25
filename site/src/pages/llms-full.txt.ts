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
      `Benchmarks (${fingerprint.captured_date}): ${SITE_URL}/benchmarks/`,
      `- Geometric mean across shared fixtures: \`ayaml value\` is ${HEADLINE.vsYamlRust2.toFixed(2)}x faster than \`yaml-rust2\`, ${HEADLINE.vsSaphyr.toFixed(2)}x faster than \`saphyr\`, ${HEADLINE.vsYamlSerde.toFixed(2)}x faster than \`yaml-serde\` (${HEADLINE.vsYamlSerdeAllocs.toFixed(1)}x fewer allocations, ${HEADLINE.vsYamlSerdePeak.toFixed(2)}x lower peak heap), and ${HEADLINE.vsSerdeSaphyr.toFixed(2)}x faster than \`serde-saphyr\`; \`ayaml events\` is ${HEADLINE.vsLibyamlEvents.toFixed(2)}x faster than C \`libyaml\` and ${HEADLINE.vsLibfyamlEvents.toFixed(2)}x faster than C \`libfyaml\`.`,
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
