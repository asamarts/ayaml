#!/usr/bin/env node
// Verifies every internal href and #fragment across site/dist/**/*.html
// respecting Astro's `base: '/ayaml'` prefix for GitHub Pages deployment.

import { existsSync, readdirSync, readFileSync, statSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const SITE_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const DIST = join(SITE_ROOT, 'dist');
const SITE_ORIGIN = 'https://asamarts.github.io';
const BASE_PREFIX = '/ayaml';

function* walkHtml(dir) {
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    const p = join(dir, entry.name);
    if (entry.isDirectory()) yield* walkHtml(p);
    else if (entry.isFile() && p.endsWith('.html')) yield p;
  }
}

function htmlPathToUrl(htmlPath) {
  const rel = htmlPath.slice(DIST.length).replace(/\\/g, '/');
  const route = rel.endsWith('/index.html') ? rel.slice(0, -'index.html'.length) : rel;
  return `${BASE_PREFIX}${route}`;
}

function stripBase(pathname) {
  if (pathname === BASE_PREFIX || pathname === `${BASE_PREFIX}/`) return '/';
  if (pathname.startsWith(`${BASE_PREFIX}/`)) return pathname.slice(BASE_PREFIX.length);
  return null;
}

function resolveDistFile(pathname) {
  const sub = stripBase(pathname);
  if (sub === null) return null;
  if (sub === '/') {
    const idx = join(DIST, 'index.html');
    return existsSync(idx) ? idx : null;
  }
  const rel = sub.replace(/^\//, '');
  const direct = join(DIST, rel);
  if (sub.endsWith('/')) {
    const idx = join(direct, 'index.html');
    if (existsSync(idx)) return idx;
    const html = `${direct.replace(/\/$/, '')}.html`;
    return existsSync(html) ? html : null;
  }
  if (existsSync(direct) && statSync(direct).isFile()) return direct;
  const idx = join(direct, 'index.html');
  if (existsSync(idx)) return idx;
  const html = `${direct}.html`;
  if (existsSync(html)) return html;
  return null;
}

const idCache = new Map();
function idsOf(htmlPath) {
  if (!idCache.has(htmlPath)) {
    const html = readFileSync(htmlPath, 'utf8');
    const ids = new Set();
    for (const m of html.matchAll(/\s(?:id|name)=["']([^"']+)["']/g)) {
      ids.add(m[1].replace(/&amp;/g, '&'));
    }
    idCache.set(htmlPath, ids);
  }
  return idCache.get(htmlPath);
}

function main() {
  if (!existsSync(DIST)) {
    console.error(`FAIL: ${DIST} does not exist; run npm run build first.`);
    process.exit(2);
  }

  const broken = new Map();
  const badFragments = new Map();
  let totalPages = 0;
  let totalLinks = 0;
  let totalFragments = 0;

  for (const htmlPath of walkHtml(DIST)) {
    totalPages++;
    const html = readFileSync(htmlPath, 'utf8');
    const sourceUrl = htmlPathToUrl(htmlPath);

    for (const match of html.matchAll(/href=["']([^"']+)["']/gi)) {
      const raw = match[1].replace(/&amp;/g, '&').trim();
      if (!raw || raw.startsWith('mailto:') || raw.startsWith('javascript:') || raw.startsWith('data:')) {
        continue;
      }
      if (raw.startsWith('//')) continue;

      let urlPath;
      let fragment = '';
      if (raw.startsWith('#')) {
        urlPath = sourceUrl;
        fragment = raw.slice(1);
      } else if (raw.startsWith('http://') || raw.startsWith('https://')) {
        if (!raw.startsWith(`${SITE_ORIGIN}${BASE_PREFIX}/`) && raw !== `${SITE_ORIGIN}${BASE_PREFIX}`) {
          continue;
        }
        const parsed = new URL(raw);
        urlPath = parsed.pathname;
        fragment = parsed.hash.replace(/^#/, '');
      } else {
        const parsed = new URL(raw, `${SITE_ORIGIN}${sourceUrl}`);
        urlPath = parsed.pathname;
        fragment = parsed.hash.replace(/^#/, '');
      }

      totalLinks++;
      const targetFile = resolveDistFile(urlPath);
      if (!targetFile) {
        if (!broken.has(urlPath)) broken.set(urlPath, new Set());
        broken.get(urlPath).add(sourceUrl);
        continue;
      }

      if (fragment && targetFile.endsWith('.html')) {
        totalFragments++;
        let decoded = fragment;
        try {
          decoded = decodeURIComponent(fragment);
        } catch {
          decoded = fragment;
        }
        if (decoded !== 'top' && decoded !== '_top' && !idsOf(targetFile).has(decoded)) {
          const key = `${urlPath}#${decoded}`;
          if (!badFragments.has(key)) badFragments.set(key, new Set());
          badFragments.get(key).add(sourceUrl);
        }
      }
    }
  }

  let failed = false;
  if (broken.size > 0) {
    failed = true;
    console.error(`FAIL: ${broken.size} broken internal link(s):`);
    for (const [href, sources] of [...broken.entries()].sort()) {
      console.error(`  - ${href} (from ${[...sources].slice(0, 4).join(', ')})`);
    }
  }
  if (badFragments.size > 0) {
    failed = true;
    console.error(`FAIL: ${badFragments.size} broken fragment anchor(s):`);
    for (const [key, sources] of [...badFragments.entries()].sort()) {
      console.error(`  - ${key} (from ${[...sources].slice(0, 4).join(', ')})`);
    }
  }

  // Also verify llms.txt and llms-full.txt were emitted.
  for (const extra of ['llms.txt', 'llms-full.txt']) {
    if (!existsSync(join(DIST, extra))) {
      failed = true;
      console.error(`FAIL: missing expected build output dist/${extra}`);
    }
  }

  if (failed) process.exit(1);
  console.log(
    `OK: verified ${totalLinks} internal links and ${totalFragments} fragments across ${totalPages} pages (+ llms.txt & llms-full.txt)`,
  );
}

main();
