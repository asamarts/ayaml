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
  let inHtmlComment = false;

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
    if (inHtmlComment) {
      if (trimmed.includes('-->')) inHtmlComment = false;
      continue;
    }
    if (trimmed.startsWith('<!--')) {
      if (!trimmed.includes('-->')) inHtmlComment = true;
      if (para.length > 0) break;
      continue;
    }
    if (!trimmed) {
      if (para.length > 0) break;
      continue;
    }
    if (trimmed.startsWith('#') || trimmed.startsWith('|') || trimmed.startsWith('>')) {
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
        let k = j + 1;
        let parenDepth = 1;
        let titleQuote = null;
        while (k < n && parenDepth > 0) {
          const c = line[k];
          if (c === '\\' && k + 1 < n) {
            k += 2;
            continue;
          }
          if (titleQuote) {
            if (c === titleQuote) titleQuote = null;
          } else if (c === '"' || c === "'") {
            titleQuote = c;
          } else if (c === '(') {
            parenDepth++;
          } else if (c === ')') {
            parenDepth--;
          }
          k++;
        }
        if (parenDepth === 0) {
          const closeParen = k - 1;
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

const CHAR_LITERAL_RE = /^'(?:\\(?:x[0-9a-fA-F]{2}|u\{[0-9a-fA-F_]+\}|.)|[^'\\])'/;

/**
 * Advance a minimal Rust lexical state across one line, tracking nested block comments,
 * line comments, character literals, raw string literals (`r#"..."#`), and normal strings
 * so rustdoc `# ` stripping never desynchronizes on quotes inside comments or raw strings.
 */
function advanceRustLexer(line, state) {
  const n = line.length;
  let i = 0;
  while (i < n) {
    if (state.stringMode === 'normal') {
      if (line[i] === '\\') {
        i += 2;
        continue;
      }
      if (line[i] === '"') {
        state.stringMode = null;
      }
      i++;
      continue;
    }
    if (state.stringMode === 'raw') {
      if (line[i] === '"') {
        let hashes = 0;
        while (hashes < state.rawHashes && line[i + 1 + hashes] === '#') {
          hashes++;
        }
        if (hashes === state.rawHashes) {
          state.stringMode = null;
          i += 1 + hashes;
          continue;
        }
      }
      i++;
      continue;
    }
    if (state.blockCommentDepth > 0) {
      if (line[i] === '/' && line[i + 1] === '*') {
        state.blockCommentDepth++;
        i += 2;
        continue;
      }
      if (line[i] === '*' && line[i + 1] === '/') {
        state.blockCommentDepth--;
        i += 2;
        continue;
      }
      i++;
      continue;
    }

    const ch = line[i];
    if (ch === '/' && line[i + 1] === '/') {
      break;
    }
    if (ch === '/' && line[i + 1] === '*') {
      state.blockCommentDepth++;
      i += 2;
      continue;
    }
    // Raw string literal: `r"..."`, `r#"..."#`, `br#"..."#`, `cr#"..."#`.
    const prevIsIdent = i > 0 && /[A-Za-z0-9_]/.test(line[i - 1]);
    if (!prevIsIdent) {
      let rPos = -1;
      if (ch === 'r') rPos = i;
      else if ((ch === 'b' || ch === 'c') && line[i + 1] === 'r') rPos = i + 1;
      if (rPos !== -1) {
        let k = rPos + 1;
        while (k < n && line[k] === '#') k++;
        if (k < n && line[k] === '"') {
          state.stringMode = 'raw';
          state.rawHashes = k - (rPos + 1);
          i = k + 1;
          continue;
        }
      }
    }
    // Character or byte-character literal (does not match lifetimes like `'a`).
    if (ch === "'") {
      const m = CHAR_LITERAL_RE.exec(line.slice(i));
      if (m) {
        i += m[0].length;
        continue;
      }
    }
    if (ch === '"') {
      state.stringMode = 'normal';
    }
    i++;
  }
}

/**
 * Rewrite relative `.md` links (using `urlPrefix`, e.g. `"/ayaml"` or
 * `"https://asamarts.github.io/ayaml"`) and strip rustdoc hidden `# ` lines
 * inside Rust code blocks while keeping multiline string literals and comments intact.
 */
export function transformDocsMarkdown(body, relPath, urlPrefix) {
  const relDir = posix.dirname(relPath.replace(/^\.\//, ''));
  const lines = body.split('\n');
  const out = [];
  let inFence = false;
  let fenceChar = '';
  let fenceLen = 0;
  let inRustFence = false;
  let rustState = { blockCommentDepth: 0, stringMode: null, rawHashes: 0 };

  for (const line of lines) {
    const leading = line.trimStart();
    if (!inFence) {
      const m = /^(`{3,}|~{3,})(.*)$/.exec(leading);
      if (m) {
        inFence = true;
        fenceChar = m[1][0];
        fenceLen = m[1].length;
        const info = m[2].trim();
        const tokens = info.split(/[\s,]+/).filter(Boolean);
        inRustFence = tokens.some((tok) => tok.toLowerCase() === 'rust');
        rustState = { blockCommentDepth: 0, stringMode: null, rawHashes: 0 };
        // Normalize rustdoc fence attributes (e.g., `rust,should_panic`, `rust should_panic`) to `rust`.
        if (inRustFence && (tokens.length > 1 || info.includes(','))) {
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
        rustState = { blockCommentDepth: 0, stringMode: null, rawHashes: 0 };
        out.push(line);
        continue;
      }
      if (inRustFence) {
        if (rustState.stringMode === null && rustState.blockCommentDepth === 0) {
          // Rustdoc hidden lines: `#` or `# ...` (with optional leading whitespace), not `#[attr]` / `#![attr]`.
          if (/^\s*#(?:$|\s)/.test(line)) {
            continue;
          }
          // Rustdoc escaped literal hash: `## ` -> `# `.
          const unescaped = line.replace(/^(\s*)##(\s|$)/, '$1#$2');
          advanceRustLexer(unescaped, rustState);
          out.push(unescaped);
          continue;
        }
        advanceRustLexer(line, rustState);
      }
      out.push(line);
    }
  }
  return out.join('\n');
}
