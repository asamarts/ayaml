#!/usr/bin/env node
// Syncs docs/**/*.md into site/src/content/docs/docs/ with Starlight frontmatter
// and generates site/src/data/sidebar.json from docs/SUMMARY.md so `docs/` stays
// the single source of truth for GitHub browsing, rustdoc doctests, and
// the published Starlight site.

import { mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import {
  extractDescription,
  extractTitleAndBody,
  relMdToDocsSubpath,
  transformDocsMarkdown,
} from './markdown-utils.mjs';

const SITE_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const REPO_ROOT = resolve(SITE_ROOT, '..');
const DOCS_SRC = join(REPO_ROOT, 'docs');
const CONTENT_DOCS = join(SITE_ROOT, 'src', 'content', 'docs');
const DOCS_OUT = join(CONTENT_DOCS, 'docs');
const SIDEBAR_OUT = join(SITE_ROOT, 'src', 'data', 'sidebar.json');
const BASE = '/ayaml';
const EDIT_BASE = 'https://github.com/asamarts/ayaml/edit/main/docs/';

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
    const subpath = relMdToDocsSubpath(rel);
    const link = `/docs/${subpath}`;
    const item = { label, rel, link };
    pages.push(item);

    if (currentGroup) {
      currentGroup.items.push({ label, link });
    } else {
      // Top-level intro link before the first section heading.
      groups.push({ label, link });
    }
  }

  return { groups, pages };
}

function yamlQuote(str) {
  return JSON.stringify(str);
}

function main() {
  const summaryText = readFileSync(SUMMARY_PATH, 'utf8');
  const { groups, pages } = parseSummary(summaryText);

  rmSync(CONTENT_DOCS, { recursive: true, force: true });
  mkdirSync(DOCS_OUT, { recursive: true });

  for (const page of pages) {
    const srcFile = join(DOCS_SRC, page.rel);
    const raw = readFileSync(srcFile, 'utf8');
    const extracted = extractTitleAndBody(raw, page.label);
    // Avoid `<title>ayaml | ayaml</title>` on the docs root index page.
    const title =
      page.rel === 'index.md' && extracted.title.toLowerCase() === 'ayaml'
        ? 'Documentation'
        : extracted.title;
    const description = extractDescription(extracted.body, title);
    const transformed = transformDocsMarkdown(extracted.body, page.rel, BASE);
    const editUrl = `${EDIT_BASE}${page.rel}`;

    const outPath = join(DOCS_OUT, page.rel);
    mkdirSync(dirname(outPath), { recursive: true });
    const frontmatter = [
      '---',
      `title: ${yamlQuote(title)}`,
      `description: ${yamlQuote(description)}`,
      `editUrl: ${yamlQuote(editUrl)}`,
      '---',
      '',
    ].join('\n');
    writeFileSync(outPath, `${frontmatter}${transformed.trimEnd()}\n`, 'utf8');
  }

  mkdirSync(dirname(SIDEBAR_OUT), { recursive: true });
  writeFileSync(SIDEBAR_OUT, `${JSON.stringify(groups, null, 2)}\n`, 'utf8');
  console.log(`Synced ${pages.length} docs pages and generated Starlight sidebar from docs/SUMMARY.md`);
}

main();
