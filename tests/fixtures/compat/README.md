# Compatibility fixtures

These fixtures are derived from a real-world toolchain-pinning ruleset and
anonymized. They exercise the YAML constructs, scalar styles, tags, anchors,
aliases, merge keys, duplicate keys, multi-document streams, and malformed flow
syntax that the compatibility profile must handle.

`toolchain-pins.yml` is a reduced, anonymized ruleset with cross-file checks for
Node.js, Rust, Python, Ruby, JVM, and .NET toolchain pins. It covers block and flow
collections, tagged scalars, regexes, JSON/YAML extraction selectors, nested
conditions, and cross-file rules. It carries no external attribution.

`scalar-tags-merges-aliases.yml` covers scalar resolution, explicit core tags,
custom tags, anchors, aliases, and merge keys.

The three `*.expected-error.yml` files are minimal cases that the
single-document convenience API must reject.
