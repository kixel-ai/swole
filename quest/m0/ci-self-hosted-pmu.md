# [XS] Run check.yml on a self-hosted PMU runner

## Goal

`.github/workflows/check.yml` runs on a self-hosted Linux runner with an exposed hardware PMU, bringing `swole`'s merge gate into compliance with the organization's self-hosted runner rule and ensuring CI exercises `quick` and `bench` perf counter tests on every pull request.

## Plan

Decided 2026-11-26 (maintainer, `/quest-plan`, surfaced during [kixel-ai/swole#5](https://github.com/kixel-ai/swole/pull/5)):

- ✅ Move `check.yml` off GitHub-hosted `ubuntu-24.04` (which exposes no PMU, so `quick` and `bench` counter assertions silently skip in CI) onto a self-hosted runner with PMU access.

Done in the quest's PR: `check.yml` runs on `[self-hosted, swole-ci]` with `SWOLE_REQUIRE_PMU=1`, which makes the `quick` and `bench` tests fail, not skip, on a host without perf counters, and `RUST_TEST_NOCAPTURE=1`, which prints each test's "perf counters: asserting ..." line in the job log.

What remains:

- No runner serves `swole-ci` yet: the host's runner list (dotfiles' `nixos/modules/ci-runner-list.nix`) has none for kixel-ai/swole, and the one perf-enabled kixel-ai runner, `rust2-bench`, is pinned to rust2's `bench.yml`. Add `swole-ci = { id = 17; owner = "kixel-ai"; runnerName = "nixos-swole-ci"; labels = [ "swole-ci" ]; jobs = 8; group = "swole-ci"; perf = true; }`, the `kixel-ai` runner group `swole-ci` (public repositories allowed, `kixel-ai/swole` only), and the README table row.
- Once it is up, read the first `check` run's log for both `perf counters: asserting ...` lines, then delete this quest.

Recommendations:

- Keep `perf_event_paranoid` at 2: `quick` and `bench` count `instructions:u` of their own process.

## Waiting

- Maintainer (action): provision the `swole-ci` runner (it needs organization admin and a host deploy, which this agent lacks; a dotfiles quest could automate the group creation, as `rust2-bench` does for its own): add the entry above to dotfiles' `nixos/modules/ci-runner-list.nix`, create the `swole-ci` runner group in the kixel-ai organization (public repositories allowed, restricted to `kixel-ai/swole`), and deploy the host. Check it with `gh api orgs/kixel-ai/actions/runners` listing `nixos-swole-ci` online, then a rerun of swole's `check` job leaving "Waiting for a runner" and printing `perf counters: asserting`.
- Maintainer: a public repository's pull requests run fork code on this runner, unlike the queue-only runners. Who may trigger it?
  - Require approval for every outside contributor's fork runs (Recommended) - the repository setting holds fork PRs until a maintainer approves their run, which the maintainer already does for `action_required` runs; the runner group still admits only swole.
  - Move the job to a `merge_group` trigger like the queue runners - needs a merge queue, which swole's ruleset does not have, and the PR then shows no counter logs.
