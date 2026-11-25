// @ts-check
import { existsSync, readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { defineConfig } from 'astro/config';
import starlight from '@astrojs/starlight';

const SITE_ROOT = dirname(fileURLToPath(import.meta.url));
const SIDEBAR_PATH = join(SITE_ROOT, 'src', 'data', 'sidebar.json');
const sidebar = existsSync(SIDEBAR_PATH)
  ? JSON.parse(readFileSync(SIDEBAR_PATH, 'utf8'))
  : [];

export default defineConfig({
  site: 'https://asamarts.github.io',
  base: '/ayaml',
  trailingSlash: 'always',
  integrations: [
    starlight({
      title: 'ayaml',
      description:
        'Safe, fast, pure-Rust YAML 1.2.2 parser, spanned event stream, generic value model, Serde codec, and deterministic emitter.',
      favicon: '/favicon.svg',
      disable404Route: true,
      social: [
        {
          icon: 'github',
          label: 'GitHub',
          href: 'https://github.com/asamarts/ayaml',
        },
      ],
      components: {
        Header: './src/components/Header.astro',
        Footer: './src/components/DocsFooter.astro',
      },
      customCss: [
        '@fontsource-variable/inter/wght.css',
        '@fontsource/jetbrains-mono/400.css',
        '@fontsource/jetbrains-mono/600.css',
        './src/styles/tokens.css',
        './src/styles/custom.css',
      ],
      sidebar,
    }),
  ],
});
