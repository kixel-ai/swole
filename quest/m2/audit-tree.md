# [M] Audit the quest tree

## Goal

Run the `tree` audit: `quest audit tree` prints what it checks, what each finding becomes, and how to plan its next run.

## Plan

- Baseline: the first run, just after the tree moved from `kixel-ai/rust2`, fixed the milestone numbers and bare `#N` links the move left behind; they need no recheck.
- Check first: plans that describe what another quest has yet to land as if it already exists, and overlapping work between m0's counter quests, which all build on `modes-and-counters`.

## Audit

- tree

## Scheduled

- 2026-12-03 - a week after the previous run

## Required

- [After m0](/quest/m1/README.md) - runs once per milestone
