set shell := ["bash", "-euo", "pipefail", "-c"]

# List the available development commands.
default:
    @just --list

# Lint the crates, the workflows, the flake, and the quest tree.
check:
    cargo clippy --locked --workspace --all-targets -- -D warnings
    cargo fmt --all --check
    cargo shear
    cargo sort --workspace --check --no-format
    actionlint
    # Every action pins a release's commit and names its tag, which Dependabot moves; it cannot move a branch's.
    # yq parses every form of a job's, step's, or composite step's `uses`, such as a quoted key or an inline mapping.
    find .github \( -name '*.yml' -o -name '*.yaml' \) -exec yq e --no-doc '((.jobs[]? | (.uses, .steps[]?.uses)), .runs.steps[]?.uses) | select(. != null) | filename + ":" + (line | tostring) + ": " + . + " #" + line_comment' {} + | { ! grep -vE '^[^:]+:[0-9]+: (\./\S+ #.*|[^ #]+@[0-9a-f]{40} #v[0-9]\S*)$'; }
    taplo format --check
    nixfmt --check flake.nix
    quest check

# Run the tests, which count under the dev shell's Valgrind, and the harness's fixture once natively.
test *args:
    cargo test --locked --workspace --all-targets {{ args }}

# Run what CI runs: lint, then test.
ci: check test

# Run swole from the development checkout: `just run run --out metrics.json`.
run *args:
    cargo run --locked --quiet --release --bin swole -- {{ args }}

# Apply formatters without changing program behavior.
fix:
    cargo clippy --locked --fix --allow-staged --allow-dirty --workspace --all-targets
    cargo fmt --all
    cargo shear --fix
    cargo sort --workspace --no-format
    taplo format
    nixfmt flake.nix
