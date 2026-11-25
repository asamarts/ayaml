#!/usr/bin/env node
// Verifies every internal href/src, #fragment, trailing slash, GitHub repo link,
// Pagefind index, sitemap, robots.txt, 404.html, and llms*.txt bundle across site/dist/.

import { existsSync, readdirSync, readFileSync, statSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const SITE_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const REPO_ROOT = resolve(SITE_ROOT, '..');
const DIST = join(SITE_ROOT, 'dist');
const SITE_ORIGIN = 'https://asamarts.github.io';
const BASE_PREFIX = '/ayaml';
const GITHUB_REPO_PREFIX = 'https://github.com/asamarts/ayaml/';

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
  if (sub === null) return { file: null, missingSlash: false };
  if (sub === '/') {
    const idx = join(DIST, 'index.html');
    return { file: existsSync(idx) ? idx : null, missingSlash: pathname === BASE_PREFIX };
  }
  const rel = sub.replace(/^\//, '');
  const direct = join(DIST, rel);
  if (sub.endsWith('/')) {
    const idx = join(direct, 'index.html');
    if (existsSync(idx)) return { file: idx, missingSlash: false };
    const html = `${direct.replace(/\/$/, '')}.html`;
    return { file: existsSync(html) ? html : null, missingSlash: false };
  }
  if (existsSync(direct) && statSync(direct).isFile()) {
    return { file: direct, missingSlash: false };
  }
  // HTML route without trailing slash violates Astro's `trailingSlash: 'always'`.
  const idx = join(direct, 'index.html');
  if (existsSync(idx)) return { file: idx, missingSlash: true };
  const html = `${direct}.html`;
  if (existsSync(html)) return { file: html, missingSlash: true };
  return { file: null, missingSlash: false };
}

const idCache = new Map();
function idsOf(htmlPath) {
  if (!idCache.has(htmlPath)) {
    const html = readFileSync(htmlPath, 'utf8');
    const ids = new Set();
    for (const m of html.matchAll(/\sid=["']([^"']+)["']/gi)) {
      ids.add(m[1].replace(/&amp;/g, '&'));
    }
    idCache.set(htmlPath, ids);
  }
  return idCache.get(htmlPath);
}

function addError(map, key, source) {
  if (!map.has(key)) map.set(key, new Set());
  map.get(key).add(source);
}

function main() {
  if (!existsSync(DIST)) {
    console.error(`FAIL: ${DIST} does not exist; run npm run build first.`);
    process.exit(2);
  }

  const broken = new Map();
  const badFragments = new Map();
  const missingSlash = new Map();
  const badRepoLinks = new Map();
  const extraErrors = [];

  let totalPages = 0;
  let totalLinks = 0;
  let totalFragments = 0;
  let totalRepoLinks = 0;

  for (const htmlPath of walkHtml(DIST)) {
    totalPages++;
    const html = readFileSync(htmlPath, 'utf8');
    const sourceUrl = htmlPathToUrl(htmlPath);
    const is404 = htmlPath === join(DIST, '404.html');

    for (const match of html.matchAll(/\s(href|src)=["']([^"']+)["']/gi)) {
      const attr = match[1].toLowerCase();
      const raw = match[2].replace(/&amp;/g, '&').trim();
      if (
        !raw ||
        raw.startsWith('mailto:') ||
        raw.startsWith('javascript:') ||
        raw.startsWith('data:') ||
        raw.startsWith('//')
      ) {
        continue;
      }

      if (is404 && !raw.startsWith('#') && !raw.startsWith('/') && !/^https?:\/\//.test(raw)) {
        extraErrors.push(`404.html uses relative ${attr}="${raw}" (must be root-relative /ayaml/... for nested 404 paths)`);
      }

      let urlPath;
      let fragment = '';
      if (raw.startsWith('#')) {
        if (attr !== 'href') continue;
        urlPath = sourceUrl;
        fragment = raw.slice(1);
      } else if (raw.startsWith('http://') || raw.startsWith('https://')) {
        if (raw.startsWith(GITHUB_REPO_PREFIX)) {
          const parsed = new URL(raw);
          const repoMatch = /^\/asamarts\/ayaml\/(edit|blob|tree)\/main\/(.+)$/.exec(parsed.pathname);
          if (repoMatch) {
            totalRepoLinks++;
            let repoRel;
            try {
              repoRel = decodeURIComponent(repoMatch[2]).replace(/\/$/, '');
            } catch {
              addError(badRepoLinks, raw, sourceUrl);
              continue;
            }
            if (!existsSync(join(REPO_ROOT, repoRel))) {
              addError(badRepoLinks, raw, sourceUrl);
            }
          }
          continue;
        }
        if (raw.startsWith(`${SITE_ORIGIN}/`) || raw === SITE_ORIGIN) {
          const parsed = new URL(raw);
          if (parsed.pathname !== BASE_PREFIX && !parsed.pathname.startsWith(`${BASE_PREFIX}/`)) {
            addError(broken, raw, sourceUrl);
            continue;
          }
          urlPath = parsed.pathname;
          fragment = parsed.hash.replace(/^#/, '');
        } else {
          continue;
        }
      } else {
        const parsed = new URL(raw, `${SITE_ORIGIN}${sourceUrl}`);
        urlPath = parsed.pathname;
        fragment = parsed.hash.replace(/^#/, '');
      }

      totalLinks++;
      const resolved = resolveDistFile(urlPath);
      if (!resolved.file) {
        addError(broken, `${attr}:${urlPath}`, sourceUrl);
        continue;
      }
      if (attr === 'href' && resolved.missingSlash) {
        addError(missingSlash, urlPath, sourceUrl);
      }

      if (attr === 'href' && fragment && resolved.file.endsWith('.html')) {
        totalFragments++;
        let decoded = fragment;
        try {
          decoded = decodeURIComponent(fragment);
        } catch {
          decoded = fragment;
        }
        if (decoded !== 'top' && decoded !== '_top' && !idsOf(resolved.file).has(decoded)) {
          addError(badFragments, `${urlPath}#${decoded}`, sourceUrl);
        }
      }
    }
  }

  for (const extra of ['llms.txt', 'llms-full.txt', 'robots.txt', '404.html', 'sitemap-index.xml']) {
    if (!existsSync(join(DIST, extra))) {
      extraErrors.push(`missing expected build output dist/${extra}`);
    }
  }
  if (!existsSync(join(DIST, 'pagefind', 'pagefind-ui.js'))) {
    extraErrors.push('missing Pagefind index under dist/pagefind/');
  }

  // Strip fenced code blocks and inline code spans from Markdown text before prose scans.
  const stripCode = (markdown) => {
    const out = [];
    let inFence = false;
    let fenceChar = '';
    let fenceLen = 0;
    for (const line of markdown.split('\n')) {
      const trimmed = line.trimStart();
      if (!inFence) {
        const m = /^(`{3,}|~{3,})/.exec(trimmed);
        if (m) {
          inFence = true;
          fenceChar = m[1][0];
          fenceLen = m[1].length;
          continue;
        }
        out.push(line.replace(/`+[^`]*`+/g, ''));
      } else {
        const closeRe = new RegExp(`^${fenceChar}{${fenceLen},}\\s*$`);
        if (closeRe.test(trimmed)) inFence = false;
      }
    }
    return out.join('\n');
  };

  // Verify llms.txt & llms-full.txt links (base path, trailing slash, fragments) and ensure rustdoc boilerplate was stripped.
  for (const bundle of ['llms.txt', 'llms-full.txt']) {
    const p = join(DIST, bundle);
    if (!existsSync(p)) continue;
    const text = readFileSync(p, 'utf8');
    for (const m of text.matchAll(/https:\/\/asamarts\.github\.io(?:\/[^\s)>'"`]*)?/g)) {
      const cleanedUrl = m[0].replace(/[.,;:]+$/, '');
      const parsed = new URL(cleanedUrl);
      if (parsed.pathname !== BASE_PREFIX && !parsed.pathname.startsWith(`${BASE_PREFIX}/`)) {
        addError(broken, `${bundle}:${cleanedUrl}`, bundle);
        continue;
      }
      const resolved = resolveDistFile(parsed.pathname);
      if (!resolved.file) {
        addError(broken, `${bundle}:${parsed.pathname}`, bundle);
        continue;
      }
      if (resolved.missingSlash) {
        addError(missingSlash, `${bundle}:${parsed.pathname}`, bundle);
      }
      const fragment = parsed.hash.replace(/^#/, '');
      if (fragment && resolved.file.endsWith('.html')) {
        let decoded = fragment;
        try {
          decoded = decodeURIComponent(fragment);
        } catch {
          decoded = fragment;
        }
        if (decoded !== 'top' && decoded !== '_top' && !idsOf(resolved.file).has(decoded)) {
          addError(badFragments, `${bundle}:${parsed.pathname}#${decoded}`, bundle);
        }
      }
    }
    if (bundle === 'llms-full.txt') {
      let inFence = false;
      let fenceChar = '';
      let fenceLen = 0;
      let inRust = false;
      for (const line of text.split('\n')) {
        const trimmed = line.trimStart();
        if (!inFence) {
          const m = /^(`{3,}|~{3,})(.*)$/.exec(trimmed);
          if (m) {
            inFence = true;
            fenceChar = m[1][0];
            fenceLen = m[1].length;
            inRust = m[2]
              .trim()
              .split(/[\s,]+/)
              .some((tok) => tok.toLowerCase() === 'rust');
          }
        } else {
          const closeRe = new RegExp(`^${fenceChar}{${fenceLen},}\\s*$`);
          if (closeRe.test(trimmed)) {
            inFence = false;
            inRust = false;
          } else if (inRust && /^\s*#\s+(?:fn\s|use\s|Ok\(|let\s|extern\s)/.test(line)) {
            extraErrors.push(`dist/llms-full.txt contains unstripped rustdoc hidden line: ${line.trim()}`);
            break;
          }
        }
      }
      const proseOnly = stripCode(text);
      const relMd = proseOnly.match(/\]\((?!https?:\/\/|#|\/)[^)#]+\.md(?:#[^)]*)?\)/);
      if (relMd) {
        extraErrors.push(`dist/llms-full.txt contains unrewritten relative .md link: ${relMd[0]}`);
      }
    }
  }

  let failed = false;
  for (const [label, map] of [
    ['broken internal link/asset(s)', broken],
    ['broken fragment anchor(s)', badFragments],
    ['HTML route link(s) missing trailing slash', missingSlash],
    ['broken GitHub repository link(s)', badRepoLinks],
  ]) {
    if (map.size > 0) {
      failed = true;
      console.error(`FAIL: ${map.size} ${label}:`);
      for (const [href, sources] of [...map.entries()].sort()) {
        console.error(`  - ${href} (from ${[...sources].slice(0, 4).join(', ')})`);
      }
    }
  }
  for (const err of extraErrors) {
    failed = true;
    console.error(`FAIL: ${err}`);
  }

  if (failed) process.exit(1);
  console.log(
    `OK: verified ${totalLinks} internal links/assets, ${totalFragments} fragments, and ${totalRepoLinks} GitHub repo links across ${totalPages} pages (+ llms*.txt, sitemap, robots.txt, 404.html, pagefind)`,
  );
}

main();
