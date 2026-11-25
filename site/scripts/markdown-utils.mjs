// Shared Markdown utilities for syncing docs/**/*.md to Starlight and generating
// clean llms.txt / llms-full.txt bundles without duplicating link or rustdoc logic.

import { posix } from 'node:path';

/**
 * Strip leading `./` and convert a `docs/`-relative `.md` path to a URL path
 * suffix under `/docs/` (e.g., `index.md` -> `""`, `getting-started.md` -> `"getting-started/"`).
 */
export function relMdToDocsSubpath(rawRel) {
  const clean = rawRel.trim().replace(/^\.\//, '');
  if (clean === 'index.md') return '';
  if (clean.endsWith('/index.md')) return `${clean.slice(0, -'index.md'.length)}`;
  return `${clean.replace(/\.md$/, '')}/`;
}

/**
 * Extract the top-level Markdown heading (ignoring leading blank lines and
 * comment blocks) and return `{ title, body }`.
 */
export function extractTitleAndBody(markdown, fallbackTitle) {
  const lines = markdown.split('\n');
  let title = fallbackTitle;
  let bodyStart = 0;
  let inHtmlComment = false;

  for (let i = 0; i < lines.length; i++) {
    const trimmed = lines[i].trim();
    if (!trimmed) continue;
    if (inHtmlComment) {
      if (trimmed.includes('-->')) inHtmlComment = false;
      continue;
    }
    if (trimmed.startsWith('<!--')) {
      if (!trimmed.includes('-->')) inHtmlComment = true;
      continue;
    }
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
  return { title, body: lines.slice(bodyStart).join('\n') };
}

/**
 * Extract a concise plain-text meta description from the first non-heading
 * paragraph, accurately skipping fenced code blocks and avoiding trailing colons.
 */
export function extractDescription(body, title) {
  const lines = body.split('\n');
  const para = [];
  let inFence = false;
  let fenceChar = '';
  let fenceLen = 0;

  for (const line of lines) {
    const trimmed = line.trim();
    if (!inFence) {
      const m = /^(`{3,}|~{3,})/.exec(trimmed);
      if (m) {
        inFence = true;
        fenceChar = m[1][0];
        fenceLen = m[1].length;
        continue;
      }
    } else {
      const closeRe = new RegExp(`^${fenceChar}{${fenceLen},}\\s*$`);
      if (closeRe.test(trimmed)) inFence = false;
      continue;
    }
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
  if (desc.endsWith(':')) {
    desc = `${desc.slice(0, -1).trim()}.`;
  }
  if (desc.length > 160) {
    desc = `${desc.slice(0, 157).replace(/\s+\S*$/, '')}...`;
  }
  return desc;
}

function rewriteSingleTarget(rawTarget, relDir, urlPrefix) {
  const trimmed = rawTarget.trim();
  // Separate optional Markdown link title: `target.md "Title"`
  const titleMatch = /^(\S+)(\s+["'(].*)$/.exec(trimmed);
  const target = titleMatch ? titleMatch[1] : trimmed;
  const suffix = titleMatch ? titleMatch[2] : '';

  if (
    !target ||
    target.startsWith('#') ||
    target.startsWith('http://') ||
    target.startsWith('https://') ||
    target.startsWith('mailto:') ||
    target.startsWith('/')
  ) {
    return null;
  }
  const hashIdx = target.indexOf('#');
  const filePart = hashIdx >= 0 ? target.slice(0, hashIdx) : target;
  const fragment = hashIdx >= 0 ? target.slice(hashIdx + 1) : '';
  if (!filePart.endsWith('.md')) return null;

  const joined = posix.normalize(relDir === '.' ? filePart : posix.join(relDir, filePart));
  if (joined.startsWith('..')) return null;
  const subpath = relMdToDocsSubpath(joined);
  return `${urlPrefix}/docs/${subpath}${fragment ? `#${fragment}` : ''}${suffix}`;
}

/**
 * Rewrite Markdown links (including links whose label contains inline code spans
 * such as `` [`error-codes.md`](error-codes.md) ``), HTML `<a href="...md">`
 * attributes, and reference-style definitions, while leaving standalone inline
 * code spans (such as `` `[a](b.md)` ``) untouched.
 */
function rewriteProseLine(line, relDir, urlPrefix) {
  // Reference link definition: `[id]: relative.md "optional title"`
  const refMatch = /^(\s*\[[^\]]+\]:\s*)(\S+(?:\s+["'(].*)?)$/.exec(line);
  if (refMatch) {
    const rewritten = rewriteSingleTarget(refMatch[2], relDir, urlPrefix);
    if (rewritten) return `${refMatch[1]}${rewritten}`;
  }

  let out = '';
  let i = 0;
  const n = line.length;

  const findClosingBackticks = (start) => {
    let ticks = 0;
    while (start + ticks < n && line[start + ticks] === '`') ticks++;
    let j = start + ticks;
    while (j < n) {
      if (line[j] === '`') {
        let run = 0;
        while (j + run < n && line[j + run] === '`') run++;
        if (run === ticks) return j + run;
        j += run;
      } else {
        j++;
      }
    }
    return -1;
  };

  while (i < n) {
    const ch = line[i];

    // Preserve standalone inline code spans verbatim.
    if (ch === '`') {
      const end = findClosingBackticks(i);
      if (end !== -1) {
        out += line.slice(i, end);
        i = end;
        continue;
      }
      out += ch;
      i++;
      continue;
    }

    // Inline Markdown link: `[label](target)` where label may hold inline code or nested `[...]`.
    if (ch === '[' && (i === 0 || line[i - 1] !== '\\')) {
      let j = i + 1;
      let depth = 1;
      while (j < n && depth > 0) {
        if (line[j] === '\\' && j + 1 < n) {
          j += 2;
          continue;
        }
        if (line[j] === '`') {
          const end = findClosingBackticks(j);
          if (end !== -1) {
            j = end;
            continue;
          }
        }
        if (line[j] === '[') depth++;
        else if (line[j] === ']') depth--;
        j++;
      }
      if (depth === 0 && j < n && line[j] === '(') {
        const closeParen = line.indexOf(')', j + 1);
        if (closeParen !== -1) {
          const label = line.slice(i, j);
          const rawTarget = line.slice(j + 1, closeParen);
          const rewritten = rewriteSingleTarget(rawTarget, relDir, urlPrefix);
          if (rewritten) {
            out += `${label}(${rewritten})`;
            i = closeParen + 1;
            continue;
          }
        }
      }
    }

    // HTML `href="target"` or `href='target'` outside inline code spans.
    if ((ch === 'h' || ch === 'H') && /^href=["']/i.test(line.slice(i, i + 6))) {
      const quote = line[i + 5];
      const endQuote = line.indexOf(quote, i + 6);
      if (endQuote !== -1) {
        const rawTarget = line.slice(i + 6, endQuote);
        const rewritten = rewriteSingleTarget(rawTarget, relDir, urlPrefix);
        if (rewritten) {
          out += `${line.slice(i, i + 6)}${rewritten}${quote}`;
          i = endQuote + 1;
          continue;
        }
      }
    }

    out += ch;
    i++;
  }

  return out;
}

/**
 * Rewrite relative `.md` links (using `urlPrefix`, e.g. `"/ayaml"` or
 * `"https://asamarts.github.io/ayaml"`) and strip rustdoc hidden `# ` lines
 * inside Rust code blocks while keeping multiline string literals intact.
 */
export function transformDocsMarkdown(body, relPath, urlPrefix) {
  const relDir = posix.dirname(relPath.replace(/^\.\//, ''));
  const lines = body.split('\n');
  const out = [];
  let inFence = false;
  let fenceChar = '';
  let fenceLen = 0;
  let inRustFence = false;
  let inRustString = false;

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
        inRustString = false;
        // Strip rustdoc comma directives (e.g., `rust,should_panic`) for clean syntax highlighting.
        if (inRustFence && info.includes(',')) {
          const indent = line.slice(0, line.length - leading.length);
          out.push(`${indent}${m[1]}rust`);
        } else {
          out.push(line);
        }
        continue;
      }
      out.push(rewriteProseLine(line, relDir, urlPrefix));
    } else {
      const closeRe = new RegExp(`^${fenceChar}{${fenceLen},}\\s*$`);
      if (closeRe.test(leading)) {
        inFence = false;
        inRustFence = false;
        inRustString = false;
        out.push(line);
        continue;
      }
      if (inRustFence) {
        if (!inRustString) {
          // Rustdoc hidden lines: `#` or `# ...` (with optional leading whitespace), not `#[attr]` / `#![attr]`.
          if (/^\s*#(?:$|\s)/.test(line)) {
            continue;
          }
          // Rustdoc escaped literal hash: `## ` -> `# `.
          const unescaped = line.replace(/^(\s*)##(\s|$)/, '$1#$2');
          // Track unclosed double-quoted multiline string literals so `# comment` inside YAML strings survives.
          let escaped = false;
          for (let i = 0; i < unescaped.length; i++) {
            const ch = unescaped[i];
            if (escaped) {
              escaped = false;
              continue;
            }
            if (ch === '\\' && inRustString) {
              escaped = true;
              continue;
            }
            if (ch === '"') {
              inRustString = !inRustString;
            }
          }
          out.push(unescaped);
          continue;
        }
        // Inside multiline Rust string literal: scan for closing quote.
        let escaped = false;
        for (let i = 0; i < line.length; i++) {
          const ch = line[i];
          if (escaped) {
            escaped = false;
            continue;
          }
          if (ch === '\\') {
            escaped = true;
            continue;
          }
          if (ch === '"') {
            inRustString = !inRustString;
          }
        }
      }
      out.push(line);
    }
  }
  return out.join('\n');
}
