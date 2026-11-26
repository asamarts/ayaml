#!/usr/bin/env node
// Verifies every internal href/src, #fragment, trailing slash, GitHub repo link,
// Pagefind index, sitemap, robots.txt, 404.html, and llms*.txt bundle across site/dist/.

import { existsSync, readdirSync, readFileSync, statSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { advanceRustLexer, stripMarkdownCode } from './markdown-utils.mjs';

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

const UNSTRIPPED_RUSTDOC_LINE_RE =
  /^\s*#(?:\s+(?:fn\s|use\s|Ok(?:\(|::)|let\s|extern\s|struct\s|enum\s|const\s|static\s|type\s|trait\s|impl\s|mod\s|pub\s|\})|\s*$)/;
const EM_DASH_GLOBAL_RE = /\u2014|&mdash;|&#0*8212;|&#x0*2014;/gi;

function stripBalancedHtmlElement(html, tagName) {
  const tokenRe = new RegExp(`<(\\/)?${tagName}(?=[\\s>/])[^>]*>`, 'gi');
  let out = '';
  let last = 0;
  let depth = 0;
  for (const m of html.matchAll(tokenRe)) {
    const isClose = Boolean(m[1]);
    const isSelfClose = !isClose && m[0].endsWith('/>');
    if (!isClose) {
      if (depth === 0) {
        out += html.slice(last, m.index);
      }
      if (!isSelfClose) depth++;
      if (isSelfClose && depth === 0) {
        last = m.index + m[0].length;
      }
    } else if (depth > 0) {
      depth--;
      if (depth === 0) {
        last = m.index + m[0].length;
      }
    }
  }
  if (depth === 0) {
    out += html.slice(last);
  }
  return out;
}

/**
 * Strip non-prose HTML elements (scripts, styles, comments, code blocks, and
 * copy-button source payloads) and verify no rendered text or metadata
 * contains em dashes.
 */
export function checkHtmlNoEmDashes(html, sourceUrl) {
  // Strip <script> and <style> before HTML comments so `"<!--"` inside JS/CSS
  // strings cannot mask prose that follows the script tag.
  let prose = html
    .replace(/<script(?=[\s>/])[^>]*>[\s\S]*?<\/script\s*>/gi, '')
    .replace(/<style(?=[\s>/])[^>]*>[\s\S]*?<\/style\s*>/gi, '')
    .replace(/<!--[\s\S]*?-->/g, '');
  prose = stripBalancedHtmlElement(prose, 'pre');
  prose = stripBalancedHtmlElement(prose, 'code');
  // Strip Starlight Expressive Code (`data-code`) and custom (`data-copy`)
  // clipboard payloads mirroring fenced code blocks.
  prose = prose.replace(/\sdata-(?:code|copy)=(?:"[^"]*"|'[^']*')/gi, '');

  const errors = [];
  for (const match of prose.matchAll(EM_DASH_GLOBAL_RE)) {
    const start = Math.max(0, match.index - 30);
    const end = Math.min(prose.length, match.index + match[0].length + 30);
    const snippet = prose.slice(start, end).replace(/\s+/g, ' ').trim();
    errors.push(`${sourceUrl} contains disallowed em dash (${match[0]}): ${snippet}`);
  }
  return errors;
}

/**
 * Verify Markdown/plain text output outside code blocks and inline code spans
 * contains no em dashes or HTML em-dash entities.
 */
export function checkTextNoEmDashes(text, label) {
  const prose = stripMarkdownCode(text);
  const errors = [];
  for (const [idx, line] of prose.split('\n').entries()) {
    for (const match of line.matchAll(EM_DASH_GLOBAL_RE)) {
      errors.push(
        `${label}:${idx + 1} contains disallowed em dash (${match[0]}): ${line.trim()}`,
      );
    }
  }
  return errors;
}

const SVG_BAD_NUM_ATTR_RE =
  /\s(?:x|y|x1|x2|y1|y2|cx|cy|r|rx|ry|width|height|points|d|viewBox|transform|opacity|font-size|stroke-width)=(["'])(?:(?!\1)[^])*(?:\bNaN\b|\bInfinity\b|\bundefined\b)(?:(?!\1)[^])*\1/gi;
const SVG_NEG_DIM_ATTR_RE = /\s(?:width|height|r|rx|ry)=(["'])\s*-[^"']*\1/gi;
const SVG_BAD_TEXT_RE = /(?:^|[^A-Za-z0-9_])(?:NaN|Infinity|undefined)(?:x|%|[^A-Za-z0-9_]|$)/;

function extractBalancedSvgs(html) {
  const results = [];
  const tokenRe = /<(\/)?svg(?=[\s>/])([^>]*)>/gi;
  let depth = 0;
  let startIdx = -1;
  let openAttrs = '';
  let openEnd = -1;
  for (const m of html.matchAll(tokenRe)) {
    const isClose = Boolean(m[1]);
    const isSelfClose = !isClose && m[0].endsWith('/>');
    if (!isClose) {
      if (depth === 0) {
        startIdx = m.index;
        openAttrs = m[2];
        openEnd = m.index + m[0].length;
        if (isSelfClose) {
          results.push({ full: m[0], attrs: openAttrs, inner: '' });
          continue;
        }
      }
      if (!isSelfClose) depth++;
    } else if (depth > 0) {
      depth--;
      if (depth === 0) {
        results.push({
          full: html.slice(startIdx, m.index + m[0].length),
          attrs: openAttrs,
          inner: html.slice(openEnd, m.index),
        });
      }
    }
  }
  return results;
}

/**
 * Verify every build-time inline SVG chart (`<svg ... data-chart="...">`) has
 * accessible semantics (`role="img"`, valid `aria-labelledby`, unique IDs,
 * matching `<title>` and `<desc>`), a valid positive `viewBox`, and no
 * NaN/Infinity/undefined geometry or negative dimensions.
 */
export function checkSvgCharts(html, sourceUrl) {
  const errors = [];
  const seenChartIds = new Set();
  let count = 0;

  for (const { full, attrs, inner } of extractBalancedSvgs(html)) {
    const chartMatch = /\bdata-chart=(["'])([^"']+)\1/i.exec(attrs);
    if (!chartMatch) continue;
    count++;
    const chartKind = chartMatch[2];

    if (!/\brole=(["'])img\1/i.test(attrs)) {
      errors.push(`${sourceUrl}: SVG chart (${chartKind}) missing role="img"`);
    }

    const vbMatch = /\bviewBox=(["'])([^"']+)\1/i.exec(attrs);
    if (!vbMatch) {
      errors.push(`${sourceUrl}: SVG chart (${chartKind}) missing viewBox`);
    } else {
      const nums = vbMatch[2].trim().split(/[\s,]+/).map(Number);
      if (nums.length !== 4 || nums.some((n) => !Number.isFinite(n)) || nums[2] <= 0 || nums[3] <= 0) {
        errors.push(`${sourceUrl}: SVG chart (${chartKind}) has invalid viewBox="${vbMatch[2]}"`);
      }
    }

    const innerIds = new Set();
    for (const idm of inner.matchAll(/\bid=(["'])([^"']+)\1/gi)) {
      const idVal = idm[2];
      innerIds.add(idVal);
      if (seenChartIds.has(idVal)) {
        errors.push(`${sourceUrl}: SVG chart (${chartKind}) duplicates id "${idVal}" on page`);
      }
      seenChartIds.add(idVal);
    }

    const labelledBy = /\baria-labelledby=(["'])([^"']+)\1/i.exec(attrs);
    if (!labelledBy) {
      errors.push(`${sourceUrl}: SVG chart (${chartKind}) missing aria-labelledby`);
    } else {
      const refIds = labelledBy[2].trim().split(/\s+/).filter(Boolean);
      if (refIds.length === 0) {
        errors.push(`${sourceUrl}: SVG chart (${chartKind}) has empty aria-labelledby`);
      }
      for (const refId of refIds) {
        if (!innerIds.has(refId)) {
          errors.push(`${sourceUrl}: SVG chart (${chartKind}) aria-labelledby references missing id "${refId}"`);
        }
      }
      const titleMatch = /<title\b[^>]*\bid=(["'])([^"']+)\1[^>]*>([\s\S]*?)<\/title>/i.exec(inner);
      const descMatch = /<desc\b[^>]*\bid=(["'])([^"']+)\1[^>]*>([\s\S]*?)<\/desc>/i.exec(inner);
      if (!titleMatch || !titleMatch[3].trim()) {
        errors.push(`${sourceUrl}: SVG chart (${chartKind}) missing non-empty <title id="...">`);
      } else if (!refIds.includes(titleMatch[2])) {
        errors.push(`${sourceUrl}: SVG chart (${chartKind}) aria-labelledby does not reference title id ${titleMatch[2]}`);
      }
      if (!descMatch || !descMatch[3].trim()) {
        errors.push(`${sourceUrl}: SVG chart (${chartKind}) missing non-empty <desc id="...">`);
      } else if (!refIds.includes(descMatch[2])) {
        errors.push(`${sourceUrl}: SVG chart (${chartKind}) aria-labelledby does not reference desc id ${descMatch[2]}`);
      }
    }

    for (const bad of full.matchAll(SVG_BAD_NUM_ATTR_RE)) {
      errors.push(`${sourceUrl}: SVG chart (${chartKind}) has invalid numeric attribute: ${bad[0].trim()}`);
    }
    for (const neg of full.matchAll(SVG_NEG_DIM_ATTR_RE)) {
      errors.push(`${sourceUrl}: SVG chart (${chartKind}) has negative dimension attribute: ${neg[0].trim()}`);
    }
    for (const tm of inner.matchAll(/<text\b[^>]*>([\s\S]*?)<\/text>/gi)) {
      const textVal = tm[1].replace(/<[^>]+>/g, '').trim();
      if (SVG_BAD_TEXT_RE.test(textVal)) {
        errors.push(`${sourceUrl}: SVG chart (${chartKind}) rendered invalid text label: ${textVal}`);
      }
    }
  }
  return { count, errors };
}

/**
 * Inspect `llms-full.txt` text for unstripped rustdoc hidden lines outside
 * multiline Rust strings/comments and any unrewritten relative `.md` links
 * (inline with optional titles, reference definitions, or HTML `href`).
 */
export function checkLlmsFullContent(text) {
  const errors = [];
  let inFence = false;
  let fenceChar = '';
  let fenceLen = 0;
  let inRust = false;
  let rustState = { blockCommentDepth: 0, stringMode: null, rawHashes: 0 };
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
        rustState = { blockCommentDepth: 0, stringMode: null, rawHashes: 0 };
      }
    } else {
      const closeRe = new RegExp(`^${fenceChar}{${fenceLen},}\\s*$`);
      if (closeRe.test(trimmed)) {
        inFence = false;
        inRust = false;
        rustState = { blockCommentDepth: 0, stringMode: null, rawHashes: 0 };
      } else if (inRust) {
        if (
          rustState.stringMode === null &&
          rustState.blockCommentDepth === 0 &&
          UNSTRIPPED_RUSTDOC_LINE_RE.test(line)
        ) {
          errors.push(`dist/llms-full.txt contains unstripped rustdoc hidden line: ${line.trim()}`);
          break;
        }
        advanceRustLexer(line, rustState);
      }
    }
  }

  const proseOnly = stripMarkdownCode(text);
  const relPatterns = [
    /\]\((?!https?:\/\/|#|\/)[^)#\s]+\.md(?:#[^)\s]*)?(?:\s+[^)]*)?\)/,
    /^\s*\[[^\]]+\]:\s*(?!https?:\/\/|#|\/)\S+\.md(?:#\S*)?(?:\s+.*)?$/m,
    /\bhref=["'](?!https?:\/\/|mailto:|#|\/)[^"'#]+\.md(?:#[^"']*)?["']/i,
  ];
  for (const pattern of relPatterns) {
    const match = proseOnly.match(pattern);
    if (match) {
      errors.push(`dist/llms-full.txt contains unrewritten relative .md link: ${match[0].trim()}`);
    }
  }
  errors.push(...checkTextNoEmDashes(text, 'dist/llms-full.txt'));
  return errors;
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
  let totalSvgCharts = 0;

  for (const htmlPath of walkHtml(DIST)) {
    totalPages++;
    const html = readFileSync(htmlPath, 'utf8');
    const sourceUrl = htmlPathToUrl(htmlPath);
    const is404 = htmlPath === join(DIST, '404.html');
    extraErrors.push(...checkHtmlNoEmDashes(html, sourceUrl));
    const svgCheck = checkSvgCharts(html, sourceUrl);
    totalSvgCharts += svgCheck.count;
    extraErrors.push(...svgCheck.errors);

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
      extraErrors.push(...checkLlmsFullContent(text));
    } else {
      extraErrors.push(...checkTextNoEmDashes(text, `dist/${bundle}`));
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
  if (totalSvgCharts === 0) {
    extraErrors.push('expected rendered inline SVG benchmark charts (data-chart) in dist/, found 0');
  }
  for (const err of extraErrors) {
    failed = true;
    console.error(`FAIL: ${err}`);
  }

  if (failed) process.exit(1);
  console.log(
    `OK: verified ${totalLinks} internal links/assets, ${totalFragments} fragments, ${totalRepoLinks} GitHub repo links, and ${totalSvgCharts} SVG charts across ${totalPages} pages (+ llms*.txt, sitemap, robots.txt, 404.html, pagefind)`,
  );
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  main();
}
