import { existsSync, readFileSync } from 'node:fs';
import { join, resolve } from 'node:path';

export function findSiteRoot(): string {
  const cwd = process.cwd();
  if (existsSync(join(cwd, 'astro.config.mjs'))) return cwd;
  if (existsSync(join(cwd, 'site', 'astro.config.mjs'))) return join(cwd, 'site');
  return cwd;
}

export const SITE_ROOT = findSiteRoot();
export const REPO_ROOT = resolve(SITE_ROOT, '..');

export const SITE_ORIGIN = 'https://asamarts.github.io';
export const BASE = '/ayaml';
export const SITE_URL = `${SITE_ORIGIN}${BASE}`;
export const GITHUB_URL = 'https://github.com/asamarts/ayaml';
export const DOCS_RS_AYAML = 'https://docs.rs/ayaml';
export const DOCS_RS_EDIT = 'https://docs.rs/ayaml-edit';
export const CRATES_IO_URL = 'https://crates.io/crates/ayaml';

export function readWorkspaceVersion(): string {
  const cargoToml = readFileSync(join(REPO_ROOT, 'Cargo.toml'), 'utf8');
  const match = /^version\s*=\s*"([^"]+)"/m.exec(cargoToml);
  if (!match) {
    throw new Error('site.ts: could not read workspace version from Cargo.toml');
  }
  return match[1];
}

export const WORKSPACE_VERSION = readWorkspaceVersion();

export const NAV_ITEMS = [
  { label: 'Docs', href: `${BASE}/docs/` },
  { label: 'Benchmarks', href: `${BASE}/benchmarks/` },
  { label: 'Migrating', href: `${BASE}/docs/migrating-from/serde-yaml/` },
  { label: 'Cookbook', href: `${BASE}/docs/cookbook/` },
  { label: 'Security', href: `${BASE}/docs/security/` },
] as const;
