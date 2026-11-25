import assert from 'node:assert/strict';
import test from 'node:test';
import {
  checkHtmlNoEmDashes,
  checkLlmsFullContent,
  checkSvgCharts,
  checkTextNoEmDashes,
} from './check-links.mjs';
import {
  extractDescription,
  stripMarkdownCode,
  transformDocsMarkdown,
} from './markdown-utils.mjs';

test('transformDocsMarkdown unescapes ## inside multiline Rust strings and block comments', () => {
  const input = [
    '```rust',
    'let yaml = "\\',
    '## a YAML comment',
    'key: value',
    '";',
    '/* comment',
    '## escaped hash in block comment',
    '*/',
    '# Ok::<(), ayaml::Error>(())',
    '```',
    '',
  ].join('\n');
  const out = transformDocsMarkdown(input, 'editing.md', '/ayaml');
  assert.match(out, /^# a YAML comment$/m);
  assert.match(out, /^# escaped hash in block comment$/m);
  assert.doesNotMatch(out, /## a YAML comment/);
  assert.doesNotMatch(out, /Ok::<>/);
  assert.doesNotMatch(out, /# Ok::/);
});

test('transformDocsMarkdown handles non-BMP Unicode char literals without desynchronizing', () => {
  const input = [
    '```rust',
    "let pair = ('🦀', '\"');",
    '# fn main() {}',
    '# Ok(())',
    '```',
    '',
  ].join('\n');
  const out = transformDocsMarkdown(input, 'editing.md', '/ayaml');
  assert.match(out, /let pair = \('🦀', '"'\);/);
  assert.doesNotMatch(out, /# fn main/);
  assert.doesNotMatch(out, /# Ok\(\(\)\)/);
});

test('transformDocsMarkdown rewrites links containing unspaced apostrophes in fragments', () => {
  const single = "See [Guide](guide.md#don't) for details.\n";
  assert.equal(
    transformDocsMarkdown(single, 'index.md', '/ayaml'),
    "See [Guide](/ayaml/docs/guide/#don't) for details.\n",
  );

  const pair = "See [a](guide.md#don't) and [b](limits.md#can't).\n";
  assert.equal(
    transformDocsMarkdown(pair, 'index.md', '/ayaml'),
    "See [a](/ayaml/docs/guide/#don't) and [b](/ayaml/docs/limits/#can't).\n",
  );

  const withTitle = 'See [Guide](guide.md "User\'s (Guide)") now.\n';
  assert.equal(
    transformDocsMarkdown(withTitle, 'index.md', '/ayaml'),
    'See [Guide](/ayaml/docs/guide/ "User\'s (Guide)") now.\n',
  );
});

test('extractDescription ignores fenced markers inside multiline HTML comments', () => {
  const md = [
    '<!--',
    '```',
    'draft comment with fence',
    '-->',
    'First real paragraph.',
    '',
  ].join('\n');
  assert.equal(extractDescription(md, 'Fallback'), 'First real paragraph.');
});

test('stripMarkdownCode respects equal-length backtick spans', () => {
  const line = 'Use `` ` `` and see [bad](broken.md) or `bar`.';
  const stripped = stripMarkdownCode(line);
  assert.match(stripped, /\[bad\]\(broken\.md\)/);
  assert.doesNotMatch(stripped, /bar/);
});

test('checkLlmsFullContent catches titled/reference/HTML relative links and turbofish hidden lines', () => {
  assert.ok(
    checkLlmsFullContent('Use `` ` `` and see [bad](broken.md) or `bar`.\n').length > 0,
  );
  assert.ok(
    checkLlmsFullContent('See [Guide](bar.md "The Guide").\n').length > 0,
  );
  assert.ok(
    checkLlmsFullContent('[id]: bar.md "Guide"\n').length > 0,
  );
  assert.ok(
    checkLlmsFullContent('<a href="bar.md#sec">Guide</a>\n').length > 0,
  );
  assert.ok(
    checkLlmsFullContent('```rust\nlet x = 1;\n# Ok::<(), Error>(())\n```\n').length > 0,
  );
  assert.ok(
    checkLlmsFullContent('```rust\nlet x = 1;\n# }\n```\n').length > 0,
  );
  // YAML comment inside a multiline Rust raw string must not trigger a false positive.
  const validMultiline = [
    '```rust',
    'let yaml = r#"',
    '# use this setting carefully',
    '# let users override port',
    'port: 8080',
    '"#;',
    '```',
    '',
  ].join('\n');
  assert.deepEqual(checkLlmsFullContent(validMultiline), []);
});

test('extractDescription fallback avoids em dashes and trims trailing punctuation on truncation', () => {
  assert.equal(extractDescription('', 'Reference'), 'Reference documentation for ayaml.');
  const longWithComma =
    'A long opening paragraph with several items: alpha, beta, gamma, delta, epsilon, ' +
    'zeta, eta, theta, iota, kappa, lambda, mu, nu, xi, omicron, pi, rho, sigma, tau, upsilon.';
  const truncated = extractDescription(longWithComma, 'Long');
  assert.ok(truncated.endsWith('...'));
  assert.ok(!/[,;:]\.\.\.$/.test(truncated), `unexpected trailing punctuation: ${truncated}`);
});

test('checkHtmlNoEmDashes flags rendered text, entities, and bypasses while ignoring code and copy payloads', () => {
  assert.equal(checkHtmlNoEmDashes('<p>One \u2014 and two &mdash;.</p>', '/ayaml/').length, 2);
  assert.equal(checkHtmlNoEmDashes('<title>ayaml &mdash; YAML</title>', '/ayaml/').length, 1);
  assert.equal(checkHtmlNoEmDashes('<meta content="a &#08212; b">', '/ayaml/').length, 1);
  assert.equal(checkHtmlNoEmDashes('<p>Hex &#X002014; entity</p>', '/ayaml/').length, 1);
  // Script string containing "<!--" must not mask prose before a later HTML comment.
  assert.equal(
    checkHtmlNoEmDashes('<script>const x = "<!--";</script><p>Bad \u2014 prose</p><!-- end -->', '/ayaml/').length,
    1,
  );
  // Hyphenated custom element <code-block> must not be treated as <code>.
  assert.equal(
    checkHtmlNoEmDashes('<code-block>Bad \u2014 prose</code-block><code>ok</code>', '/ayaml/').length,
    1,
  );
  // Comments, scripts, styles, pre/nested code, and copy-button payloads are ignored.
  assert.deepEqual(
    checkHtmlNoEmDashes(
      '<!-- comment \u2014 --><script>const s = "\u2014";</script><style>/* \u2014 */</style>' +
        '<button data-code="let s = &quot;\u2014&quot;;" data-copy="\u2014">Copy</button>' +
        '<pre><code>let s = "\u2014";</code></pre>' +
        '<code>outer <code>inner</code> still \u2014 code</code><p>Clean prose.</p>',
      '/ayaml/',
    ),
    [],
  );
});

test('checkTextNoEmDashes and checkLlmsFullContent catch edge cases and report all occurrences', () => {
  assert.equal(checkTextNoEmDashes('Line one \u2014\nLine two &#08212; and &#x02014;\n', 'llms.txt').length, 3);
  assert.equal(checkTextNoEmDashes('```code``` \u2014 after inline triple ticks\n', 'llms.txt').length, 1);
  assert.equal(checkTextNoEmDashes('\\`escaped \u2014 prose`\n', 'llms.txt').length, 1);
  assert.equal(checkTextNoEmDashes('    ```text\nStill \u2014 prose\n', 'llms.txt').length, 1);
  assert.equal(checkLlmsFullContent('# Heading \u2014 Bad\n').length, 1);
  assert.deepEqual(
    checkTextNoEmDashes(
      'Inline `\u2014` and \\\\`\u2014` ok\n\nMultiline `code\n\u2014 span` ok\n\n   ```text\n\u2014 in fence\n   ```  \n',
      'llms.txt',
    ),
    [],
  );
});

test('checkSvgCharts validates accessibility semantics and rejects invalid geometry or text', () => {
  const valid =
    '<svg data-chart="diverging-ratio" role="img" aria-labelledby="c-t c-d" viewBox="0 0 760 200">' +
    '<title id="c-t">Chart title</title><desc id="c-d">Chart summary</desc>' +
    '<rect x="10" y="20" width="100" height="14" /><text x="10" y="14">1.40x</text></svg>';
  assert.deepEqual(checkSvgCharts(valid, '/ayaml/benchmarks/'), { count: 1, errors: [] });

  // Non-chart icon SVGs are ignored.
  assert.deepEqual(checkSvgCharts('<svg width="16" height="16"><path d="M0 0"/></svg>', '/ayaml/'), {
    count: 0,
    errors: [],
  });

  // NaN / Infinity attributes, negative dimensions, bad viewBox, bad text, and missing semantics fail.
  const broken =
    '<svg data-chart="scaling-plot" viewBox="0 0 -760 NaN">' +
    '<rect x="10" width="-5" height="Infinity" transform="translate(NaN,0)" />' +
    '<polygon points="0,0 NaN,5 1,1" /><text x="1" y="1">NaNx</text></svg>';
  const res = checkSvgCharts(broken, '/ayaml/benchmarks/');
  assert.equal(res.count, 1);
  assert.ok(res.errors.some((e) => e.includes('role="img"')));
  assert.ok(res.errors.some((e) => e.includes('aria-labelledby')));
  assert.ok(res.errors.some((e) => e.includes('invalid viewBox')));
  assert.ok(res.errors.some((e) => e.includes('negative dimension')));
  assert.ok(res.errors.some((e) => e.includes('height="Infinity"')));
  assert.ok(res.errors.some((e) => e.includes('transform="translate(NaN,0)"')));
  assert.ok(res.errors.some((e) => e.includes('points="0,0 NaN,5 1,1"')));
  assert.ok(res.errors.some((e) => e.includes('invalid text label: NaNx')));

  // Dangling aria-labelledby IDs and duplicate chart IDs on one page fail.
  const danglingAndDup =
    '<svg data-chart="grouped-bar" role="img" aria-labelledby="t d missing-id" viewBox="0 0 100 100">' +
    '<title id="t">Title</title><desc id="d">Desc</desc></svg>' +
    '<svg data-chart="grouped-bar" role="img" aria-labelledby="t d" viewBox="0 0 100 100">' +
    '<title id="t">Title 2</title><desc id="d">Desc 2</desc></svg>';
  const dupRes = checkSvgCharts(danglingAndDup, '/ayaml/benchmarks/');
  assert.ok(dupRes.errors.some((e) => e.includes('missing id "missing-id"')));
  assert.ok(dupRes.errors.some((e) => e.includes('duplicates id "t"')));
});
