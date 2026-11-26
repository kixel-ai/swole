# [XS] Run check.yml on a self-hosted PMU runner

## Goal

`.github/workflows/check.yml` runs on a self-hosted Linux runner with an exposed hardware PMU, bringing `swole`'s merge gate into compliance with the organization's self-hosted runner rule and ensuring CI exercises `quick` and `bench` perf counter tests on every pull request.

## Plan

Decided 2026-11-26 (maintainer, `/quest-plan`, surfaced during [kixel-ai/swole#5](https://github.com/kixel-ai/swole/pull/5)):

- ✅ Move `check.yml` off GitHub-hosted `ubuntu-24.04` (which exposes no PMU, so `quick` and `bench` counter assertions silently skip in CI) onto a self-hosted runner with PMU access.

Recommendations for the implementer:

- Update `runs-on` in `.github/workflows/check.yml` to match the organization's self-hosted runner label (see dotfiles' `bin/repo-settings` and sibling repos like `moq` / `rust2`).
- Verify in CI logs on the PR that `counts_with_perf_counters_what_valgrind_does` and `times_each_harness_benchmark`'s perf counter assertions actually execute rather than returning early on a PMU-less host.
- Keep `just ci` green and ensure action pins pass the `check` recipe's `yq` pin gate.

## Required

- [Three modes (quick, sim, bench), capability probe, CAP_PERFMON, L2/L1 perf, and OS rusage](/quest/m0/modes-and-counters.md) - adds the `quick` and `bench` perf counter tests CI needs to exercise
