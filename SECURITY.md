# Security policy

## Reporting a vulnerability

Please report vulnerabilities privately through
[GitHub's private reporting](https://github.com/asamarts/ayaml/security/advisories/new),
not in a public issue or pull request. Include the ayaml version, the entry point
and any non-default `Limits`, and an input or test that shows the problem.

A maintainer replies through the advisory. Fixes ship in a patch release, and the
advisory and changelog describe the problem once that release is available.

## Supported versions

Only the latest `0.1.x` release gets security fixes. A fix isn't backported to an
older patch release, so please upgrade.

## What counts

ayaml is meant to parse untrusted input, so these are in scope when they happen
with the default `Limits`, or with limits you've tightened:

- a panic, abort, or stack overflow on any input;
- CPU or memory use out of proportion to the input, or a way around a limit;
- a read of memory or files, or code execution, caused by input; and
- a document that decodes to values it doesn't contain, without an error.

Raising or removing limits is your choice, so resource use that results from doing
so isn't a vulnerability. [`docs/limits.md`](docs/limits.md) describes each limit.

Error messages can include text from the document, so don't log them unfiltered
when documents hold secrets.
