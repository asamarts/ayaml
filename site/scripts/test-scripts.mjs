import assert from 'node:assert/strict';
import test from 'node:test';
import { checkLlmsFullContent } from './check-links.mjs';
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
