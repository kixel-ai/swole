# [XL] Audit agent instructions

## Goal

Run the `instructions` audit: `quest audit instructions` prints what it checks, what each finding becomes, and how to plan its next run.

## Plan

Baseline from the first run, [kixel-ai/swole#7](https://github.com/kixel-ai/swole/pull/7):

- It read through 2026-11-26 at `863128c`, the setup commit. Nothing ran in swole before 2026-11-25, the day the repository was created.
- `quest messages --since 2026-10-25` ran in a swole checkout on desktop, fedora, mini, nixos, and kixtop. It dropped entries from directories no origin names: 51 on desktop, 5 on fedora, 1 on mini, 2 on nixos, and 6 on kixtop.
- Loaded sizes:
  - `AGENTS.md`: 1,675 bytes after the run (1,833 before).
  - `CONTRIBUTING.md`: 993 bytes, read on demand.
  - Skill stubs: 8 per harness, 827 description characters in all.
  - Claude Code loads the root `AGENTS.md` natively, since no `CLAUDE.md` exists. Codex joins it with the machine's 3,980-byte `~/.codex/AGENTS.md`, about 5.7 KB of its 32 KiB `project_doc_max_bytes`.
- Guidance was read 2026-11-26: every page `quest guide prompting` links, plus Claude's Opus 5.5 prompting page and Claude Code's permissions page. The newest-GPT page covers `gpt-6.2-astra`, while `quest review` runs `gpt-6.2-sol`.
- No rule was kept against evidence. Every earlier swole session was still running, none ran Codex, and no review record existed.

Out of scope: rust2's history before the move belongs to rust2's own audit. The skill stubs and the `Quests:` line in `AGENTS.md` are `quest`'s, fixed at its source.

Check first:

- whether m0's modes (`swole quick|sim|bench`) reworded `AGENTS.md`'s count rules and opening line;
- the first Codex sessions and review records here;
- if a `RISK.md` arrives, that no `## Automatic` class covers `AGENTS.md`, `CONTRIBUTING.md`, `.claude/`, or `.agents/`.

## Audit

- instructions

## Scheduled

- 2026-12-03 - a week after the previous run

## Required

- [After m0](/quest/m1/README.md) - runs once per milestone
