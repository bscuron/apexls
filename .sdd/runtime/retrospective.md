# Run Retrospective

**INTERIM - run in progress** (trigger: mid-run). This snapshot was generated before all tasks reached a terminal state and will be overwritten by the final retrospective at orchestrator shutdown.

Generated: 2026-09-05 02:13:20

## Overview

- **Completion rate:** 0% (0 done / 2 total)
- **Failed tasks:** 2
- **Total cost:** $0.0000
- **Wall-clock duration:** 3s

## Run Health

- **Verdict:** UNHEALTHY

| Terminator category | Count |
|----------------------|-------|
| Agent-completed | 0 |
| Agent-reported failure | 1 |
| Watchdog-killed | 0 |
| Janitor-rejected | 0 |
| Timeout-killed | 0 |
| Auto-completed after agent death | 0 |
| Other forced termination | 0 |
| Declared task unfinished (no output / never terminated) | 1 |
| Unresolved in metrics (started, outcome never reconciled) | 0 |
| Merge refused by guard (agent work discarded) | 0 |

- **Warning:** 1 declared task(s) never reached a terminal outcome (neither done nor failed) -- the run ended while they were still open/claimed/in-progress. The goal was not met. A common cause is an agent that produced no model output (0 tokens) and was reaped -- the model may have rejected the request (context/rate limit). Check .sdd/runtime/*.log for the agent transcript.

- **Warning:** 2/2 task terminations were NOT genuine agent completions (failed/watchdog/timeout/janitor/other-forced/auto-completed/unresolved) - the completion rate above does not reflect real progress. Investigate the dominant non-agent category before trusting this run's outcome.

## Failure Analysis

### By role

| Role | Done | Failed | Total | Failure rate |
|------|------|--------|-------|--------------|
| manager | 0 | 1 | 1 | 100% |

### By complexity

| Complexity | Done | Failed | Total | Failure rate |
|------------|------|--------|-------|--------------|
| high | 0 | 1 | 1 | 100% |

### Failed task titles

- Plan and decompose goal into tasks *(role: manager, complexity: high)*

## Performance

## Cost Breakdown

## Agent Summary

*(No in-memory agent metrics available.)*

## Recommendations

- UNHEALTHY: most task terminations were non-agent-caused (dominant cause: agent_reported_failure, see Run Health table) - do not trust the completion rate; diagnose the forcing mechanism (watchdog/timeout/janitor) before re-running.
- Overall failure rate is 100% - review task definitions and agent prompts before the next run.
