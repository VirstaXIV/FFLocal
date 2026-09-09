---
version: "0.1.2"
level: copilot
processes:
  design: copilot
  implementation: copilot
  testing: pair
  documentation: copilot
  review: pair
  deployment: copilot
---

This format is based on [AI-DECLARATION.md](https://ai-declaration.md/en/0.1.2).

## Notes

- The engine is implemented by Claude Code sessions directed by the maintainer; commits
  carry `Co-Authored-By: Claude` trailers.
- Format research (FFXIV and FFXI file formats, animation/collision/sound layouts) is done
  by the same sessions, cross-checked against the vendored [Physis](https://github.com/redstrate/Physis)
  reference implementation and verified with in-repo tooling (`ffl-cli`, screenshot/trace
  aids) rather than taken on faith.
- The maintainer decides scope and priorities, verifies every change against the running
  game before it ships, and authorizes all releases.
- `vendor/physis` is a third-party project (patched; see `patches/`) and not covered by
  this declaration.
