# Performance engineering

DeviceRail treats performance claims as measured results tied to a workload,
build profile, machine, and commit. The project does not describe any component
as “maximum performance” without a reproducible benchmark and profiler trace.

## Event Store load baseline

The built-in load benchmark exercises concurrent appends across independent
Sessions, bounded page reads, latency percentiles, throughput, and peak RSS:

```bash
DEVICERAIL_PERF_OUTPUT=target/event-store-load.json \
  cargo bench -p devicerail-core --bench event_store_load -- --smoke
python3 scripts/check-performance.py target/event-store-load.json
```

Omit `--smoke` for the 32-Session, 160,000-operation local profile. Run the
release binary on an otherwise idle machine, retain the JSON with the commit,
OS, architecture, CPU, and available memory, and compare like-for-like runs.
The smoke thresholds are deliberately broad CI regression guardrails, not a
published capacity claim or service-level objective.

## Profiling

Install `cargo-flamegraph` outside the repository, then profile the full load:

```bash
cargo flamegraph -p devicerail-core --bench event_store_load
```

For daemon end-to-end load, start a release daemon with the intended Driver and
transport, record request throughput plus p50/p95/p99 latency and RSS, and keep
the raw request/result data. Never compare debug and release builds or results
from different Driver/device topologies as if they were equivalent.

## Performance invariants covered by tests

- independent in-memory Sessions do not share one append mutex;
- Event Store page snapshots clone bounded `Arc` handles under the Session lock
  and clone event bodies after releasing it;
- Recorder pages append to a checksum journal and do not rewrite the durable
  event prefix; recovery validates the chain and sealing compacts once;
- Evidence object hashing runs outside the global mutation gate, while GC and
  reference publication remain serialized for crash consistency;
- distributed peer discovery, shard inventory, and health probes run
  concurrently and return deterministic ordered results;
- paged Session export measures each candidate event once and materializes only
  the selected response.

These invariants prevent known algorithmic regressions. Capacity and tail
latency still depend on the deployment and must be measured there.
