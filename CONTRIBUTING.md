# Commits

PRs into `main` land as squash commits, so the PR title becomes the commit subject.

Use conventional-commit subjects (`feat: ...`, `fix(harness): ...`, `chore: ...`, `docs: ...`).

# PRs

Keep the body short and structured, not narrated.
Have at least these sections:

- **Problem**: a summary of the problem and why this PR is needed.
- **Approach**: a summary of the approach taken to solve the problem.
- **Impact**: a bullet point for every public API change, including the harness's attribute, the command line, `metrics.json`, and what changes a count's fingerprint.
- **Alternatives**: any alternative approaches considered.
- **Follow-ups**: the quests it adds, and work for another repository's quest tree, named with that repository.

# Landing

A PR lands once `just ci` passes locally on the head it lands and its `quest review` rounds pass, since CI runs no checks on pull requests; queue the reviewed head with `quest merge --squash --land <reviewed head's full SHA> <pr>`.
The admin bypass of `main`'s rulesets is for emergencies.
