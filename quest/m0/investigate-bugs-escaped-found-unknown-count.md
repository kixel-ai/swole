# [M] Investigate bugs/escaped/found:unknown (count), conflicts/events (count), and 5 more

## Goal

Kixel Quest measured what the Plan lists and filed it here on its own. For each finding:

1. Diagnose its cause, starting from the culprit's diff and sessions where it has them.
2. Have a second session, one that did not write the diagnosis, try to refute it, and diagnose again when it does.
3. Fix it, accept it with the reason recorded in this Plan, or improve on it further. A fix carries the measurement that shows it.

Then look past the finding: make the related improvements the diagnosis turns up where they fit here, and add the rest as follow-up quests.

## Plan

Each finding keeps the comment Kixel Quest finds it by, so a later run that sees it again adds its sighting here instead of filing it anew.

### `bugs/escaped/found:unknown (count)`: bottleneck

<!-- kixel.quest finding {"series":"bugs/escaped/found:unknown (count)","culprit":null} -->

- Series: [`bugs/escaped/found:unknown (count)`](https://kixel.quest/factory#bottlenecks-1472738961).
- 2026-11-28: 0 against a baseline of 0, +0. The largest share of escaped bugs, 100% of its measure over the 30 days to 2026-11-28; its last 7 days ran no different than the 30 before, median 0 over 7 days against 0 over 30, one-sided Mann-Whitney U, p = 0.1892.

### `conflicts/events (count)`: bottleneck

<!-- kixel.quest finding {"series":"conflicts/events (count)","culprit":null} -->

- Series: [`conflicts/events (count)`](https://kixel.quest/factory#bottlenecks-1472738961).
- 2026-11-28: 0 against a baseline of 0, +0. The largest share of conflicts, 16.67% of PRs over the 30 days to 2026-11-28; too few measurements for a trend.

### `cost/by-model/model:claude-opus-5-5 (USD)`: bottleneck

<!-- kixel.quest finding {"series":"cost/by\u002dmodel/model:claude\u002dopus\u002d5\u002d5 (USD)","culprit":null} -->

- Series: [`cost/by-model/model:claude-opus-5-5 (USD)`](https://kixel.quest/factory#bottlenecks-1472738961), in USD.
- 2026-11-28: 2.596 USD against a baseline of 2.596 USD, +0 USD (+0%). The largest share of cost, 81.43% of its measure over the 30 days to 2026-11-28; too few measurements for a trend.

### `host/gate-wait/command:nix develop (h)`: bottleneck

<!-- kixel.quest finding {"series":"host/gate\u002dwait/command:nix develop (h)","culprit":null} -->

- Series: [`host/gate-wait/command:nix develop (h)`](https://kixel.quest/factory#bottlenecks-1472738961), in h.
- 2026-11-28: 0.00001472 h against a baseline of 0.00001472 h, +0 h (+0%). The largest share of host time, 83.12% of its measure over the 30 days to 2026-11-28; too few measurements for a trend.

### `reviews/refuted/reviewer:Codex, gpt-6.2-sol, high effort (count)`: bottleneck

<!-- kixel.quest finding {"series":"reviews/refuted/reviewer:Codex, gpt\u002d6.2\u002dsol, high effort (count)","culprit":null} -->

- Series: [`reviews/refuted/reviewer:Codex, gpt-6.2-sol, high effort (count)`](https://kixel.quest/factory#bottlenecks-1472738961).
- 2026-11-28: 0 against a baseline of 0, +0. The largest share of reviews, 100% of its measure over the 30 days to 2026-11-28; too few measurements for a trend.

### `rework/fixes (count)`: bottleneck

<!-- kixel.quest finding {"series":"rework/fixes (count)","culprit":null} -->

- Series: [`rework/fixes (count)`](https://kixel.quest/factory#bottlenecks-1472738961).
- 2026-11-28: 0 against a baseline of 0, +0. The largest share of rework, 4.167% of PRs over the 30 days to 2026-11-28; too few measurements for a trend.

### `ship/pickup (h)`: bottleneck

<!-- kixel.quest finding {"series":"ship/pickup (h)","culprit":null} -->

- Series: [`ship/pickup (h)`](https://kixel.quest/factory#bottlenecks-1472738961), in h.
- 2026-11-28: 9.227 h against a baseline of 9.227 h, +0 h (+0%). The largest share of time to ship, 31.6% of its phases over the 30 days to 2026-11-28; too few measurements for a trend.
