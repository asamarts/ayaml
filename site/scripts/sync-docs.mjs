#!/usr/bin/env node
// Syncs docs/**/*.md into site/src/content/docs/docs/ with Starlight frontmatter
// and generates site/src/data/sidebar.json from docs/SUMMARY.md so `docs/` stays
// the single source of truth for GitHub browsing, rustdoc book_doctests, and
// the published Starlight site.

import { mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { dirname, join, posix, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const SITE_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const REPO_ROOT = resolve(SITE_ROOT, '..');
const DOCS_SRC = join(REPO_ROOT, 'docs');
const DOCS_OUT = join(SITE_ROOT, 'src', 'content', 'docs', 'docs');
const SIDEBAR_OUT = join(SITE_ROOT, 'src', 'data', 'sidebar.json');
const BASE = '/ayaml';

const SUMMARY_PATH = join(DOCS_SRC, 'SUMMARY.md');

function parseSummary(summaryText) {
  const groups = [];
  let currentGroup = null;
  const pages = [];

  for (const rawLine of summaryText.split('\n')) {
    const line = rawLine.trim();
    if (!line || line.startsWith('<!--')) continue;

    const heading = /^#\s+(.+)$/.exec(line);
    if (heading) {
      const label = heading[1].trim();
      if (label.toLowerCase() === 'summary') continue;
      currentGroup = { label, items: [] };
      groups.push(currentGroup);
      continue;
    }

    const linkMatch = /\[([^\]]+)\]\(([^)#]+\.md)\)/.exec(line);
    if (!linkMatch) continue;
    const label = linkMatch[1].trim();
    const rel = linkMatch[2].trim().replace(/^\.\//, '');
    const slug = rel === 'index.md' ? 'docs' : `docs/${rel.replace(/\.md$/, '')}`;
    const item = { label, rel, slug };
    pages.push(item);

    if (currentGroup) {
      currentGroup.items.push({ label, link: `/${slug}/` });
    } else {
      // Top-level intro link before the first section heading.
      groups.push({ label, link: `/${slug}/` });
    }
  }

  return { groups, pages };
}

function extractTitleAndBody(markdown, fallbackTitle) {
  const lines = markdown.split('\n');
  let title = fallbackTitle;
  let bodyStart = 0;

  for (let i = 0; i < lines.length; i++) {
    const trimmed = lines[i].trim();
    if (!trimmed) continue;
    const h1 = /^#\s+(.+)$/.exec(trimmed);
    if (h1) {
      title = h1[1].replace(/`/g, '').trim();
      bodyStart = i + 1;
    }
    break;
  }

  while (bodyStart < lines.length && lines[bodyStart].trim() === '') {
    bodyStart++;
  }
  const body = lines.slice(bodyStart).join('\n');
  return { title, body };
}

function extractDescription(body, title) {
  const lines = body.split('\n');
  const para = [];
  let inFence = false;
  for (const line of lines) {
    const trimmed = line.trim();
    if (trimmed.startsWith('```') || trimmed.startsWith('~~~')) {
      inFence = !inFence;
      continue;
    }
    if (inFence) continue;
    if (!trimmed) {
      if (para.length > 0) break;
      continue;
    }
    if (
      trimmed.startsWith('#') ||
      trimmed.startsWith('|') ||
      trimmed.startsWith('>') ||
      trimmed.startsWith('<!--')
    ) {
      if (para.length > 0) break;
      continue;
    }
    para.push(trimmed);
  }
  if (para.length === 0) {
    return `${title} — ayaml documentation.`;
  }
  let desc = para
    .join(' ')
    .replace(/\[([^\]]+)\]\([^)]+\)/g, '$1')
    .replace(/`([^`]+)`/g, '$1')
    .replace(/\*\*([^*]+)\*\*/g, '$1')
    .replace(/\*([^*]+)\*/g, '$1')
    .replace(/\s+/g, ' ')
    .trim();
  if (desc.length > 160) {
    desc = `${desc.slice(0, 157).replace(/\s+\S*$/, '')}...`;
  }
  return desc;
}

function rewriteRustFenceAndLinks(body, relPath) {
  const relDir = posix.dirname(relPath);
  const lines = body.split('\n');
  const out = [];
  let inFence = false;
  let fenceChar = '';
  let fenceLen = 0;
  let inRustFence = false;

  for (const line of lines) {
    const leading = line.trimStart();
    if (!inFence) {
      const m = /^(`{3,}|~{3,})(.*)$/.exec(leading);
      if (m) {
        inFence = true;
        fenceChar = m[1][0];
        fenceLen = m[1].length;
        const info = m[2].trim();
        inRustFence = /^rust(?=$|[\s,])/.test(info);
        // Strip rustdoc hidden-line syntax markers inside rendered code blocks.
        out.push(line);
        continue;
      }
      // Rewrite relative .md links in prose to Starlight trailing-slash URLs.
      const rewritten = line.replace(
        /(\[[^\]]*\]\()([^)]+)(\))/g,
        (full, open, rawTarget, close) => {
          const target = rawTarget.trim();
          if (
            !target ||
            target.startsWith('#') ||
            target.startsWith('http://') ||
            target.startsWith('https://') ||
            target.startsWith('mailto:') ||
            target.startsWith('/')
          ) {
            return full;
          }
          const [filePart, fragment] = target.split('#');
          if (!filePart.endsWith('.md')) {
            return full;
          }
          const joined = posix.normalize(relDir === '.' ? filePart : posix.join(relDir, filePart));
          if (joined.startsWith('..')) {
            return full;
          }
          const slug = joined === 'index.md' ? '' : `${joined.replace(/\.md$/, '')}/`;
          const url = `${BASE}/docs/${slug}${fragment ? `#${fragment}` : ''}`;
          return `${open}${url}${close}`;
        },
      );
      out.push(rewritten);
    } else {
      const closeRe = new RegExp(`^${fenceChar}{${fenceLen},}\\s*$`);
      if (closeRe.test(leading)) {
        inFence = false;
        inRustFence = false;
        out.push(line);
        continue;
      }
      // Hide rustdoc `# ` boilerplate lines in rendered site code blocks,
      // matching rustdoc/mdBook rendering while keeping docs/*.md runnable.
      if (inRustFence && (line === '#' || line.startsWith('# '))) {
        continue;
      }
      out.push(line);
    }
  }
  return out.join('\n');
}

function yamlQuote(str) {
  return JSON.stringify(str);
}

function main() {
  const summaryText = readFileSync(SUMMARY_PATH, 'utf8');
  const { groups, pages } = parseSummary(summaryText);

  rmSync(DOCS_OUT, { recursive: true, force: true });
  mkdirSync(DOCS_OUT, { recursive: true });

  for (const page of pages) {
    const srcFile = join(DOCS_SRC, page.rel);
    const raw = readFileSync(srcFile, 'utf8');
    const { title, body } = extractTitleAndBody(raw, page.label);
    const description = extractDescription(body, title);
    const transformed = rewriteRustFenceAndLinks(body, page.rel);

    const outPath = join(DOCS_OUT, page.rel);
    mkdirSync(dirname(outPath), { recursive: true });
    const frontmatter = [
      '---',
      `title: ${yamlQuote(title)}`,
      `description: ${yamlQuote(description)}`,
      '---',
      '',
    ].join('\n');
    writeFileSync(outPath, `${frontmatter}${transformed.trimEnd()}\n`, 'utf8');
  }

  mkdirSync(dirname(SIDEBAR_OUT), { recursive: true });
  writeFileSync(SIDEBAR_OUT, `${JSON.stringify(groups, null, 2)}\n`, 'utf8');
  writeFileSync(
    join(SITE_ROOT, 'src', 'content', 'docs', '404.md'),
    [
      '---',
      'title: "Page not found"',
      'template: splash',
      'editUrl: false',
      'hero:',
      '  title: "404"',
      '  tagline: "That documentation page does not exist. Try searching or return to the ayaml docs."',
      '  actions:',
      '    - text: "Go to ayaml docs"',
      '      link: /ayaml/docs/',
      '      icon: right-arrow',
      '---',
      '',
    ].join('\n'),
    'utf8',
  );
  console.log(`Synced ${pages.length} docs pages and generated Starlight sidebar from docs/SUMMARY.md`);
}

main();
