# [M] Audit the quest tree

## Goal

Run the `tree` audit: `quest audit tree` prints what it checks, what each finding becomes, and how to plan its next run.

## Plan

- Baseline: the first run, just after the tree moved from `kixel-ai/rust2`, fixed the milestone numbers and bare `#N` links the move left behind; they need no recheck.
- Check first: plans that describe what another quest has yet to land as if it already exists, and two open quests that plan the same piece of work, as m0's `sim-cycles` and `callgrind-attribution` did.

## Audit

- tree

## Scheduled

- 2026-12-03 - a week after the previous run

## Required

- [After m0](/quest/m1/README.md) - runs once per milestone
