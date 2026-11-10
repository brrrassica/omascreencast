# M0 provenance log

One row per captured file in this directory. Fill this in when the M0 spikes
are executed. See [`../README.md`](../README.md) for the field meanings.

| File | captured_at | client | server | task | method | tool | redacted |
|---|---|---|---|---|---|---|---|
| _(none yet)_ | | | | | | | |

## Notes

- `spike-capture.jsonl` is the default `--log` target of
  [`tools/m0-gamestream/fake_server.py`](../../../../tools/m0-gamestream/fake_server.py).
- `oracle-*.jsonl` are captures from the real Sunshine oracle used as a black
  box (M0-T1). Record the oracle commit in `server`; never copy its source.
- Every capture must correspond to a step in
  [`docs/m0-runbook.md`](../../../../docs/m0-runbook.md).
