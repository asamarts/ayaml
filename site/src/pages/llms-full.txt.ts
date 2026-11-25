import { readFileSync } from 'node:fs';
import { join, resolve } from 'node:path';
import { SITE_URL, WORKSPACE_VERSION } from '../lib/site';

export const prerender = true;

const REPO_ROOT = resolve(process.cwd(), '..');

export const GET = async () => {
  const summary = readFileSync(join(REPO_ROOT, 'docs', 'SUMMARY.md'), 'utf8');
  const sections: string[] = [
    `# ayaml ${WORKSPACE_VERSION} — Complete Documentation\n\nCanonical URL: ${SITE_URL}/\n`,
  ];

  for (const line of summary.split('\n')) {
    const m = /\[([^\]]+)\]\(([^)#]+\.md)\)/.exec(line);
    if (!m) continue;
    const rel = m[2].trim();
    const slug = rel === 'index.md' ? '' : `${rel.replace(/\.md$/, '')}/`;
    const content = readFileSync(join(REPO_ROOT, 'docs', rel), 'utf8').trim();
    sections.push(`\n---\nURL: ${SITE_URL}/docs/${slug}\n\n${content}\n`);
  }

  return new Response(sections.join('\n'), {
    headers: { 'Content-Type': 'text/plain; charset=utf-8' },
  });
};
