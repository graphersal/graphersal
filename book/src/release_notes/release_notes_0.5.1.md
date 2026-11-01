# Release 0.5.1

A patch release of the 0.5 line that fixes a performance regression of 0.5.0. It concerns
scripts, saved-query calls and transactions that run many writing traversals in one unit after
the graph has shrunk or grown a lot in it, such as a loader that drops most of a graph and writes
it again: in 0.5.0 each of these traversals walked the whole graph. A real loader of that kind
took 618 s with 0.5.0 and takes about 33 s with 0.5.1. The release also documents what the
estimated memory measure reports inside such a unit (see "Fixed"). It changes no format and no API; stores, snapshots and journals of 0.5.0 are read
and written exactly as before.

## Fixed

- **Writing traversals no longer walk the whole graph one after the other inside a script or
  transaction.** 0.5.0 measured what a writing traversal adds to the graph (for the
  per-execution limit and the process memory ceiling,
  [Resource Limits](../users_guide/resource_limits.md)) from the graph's average element size,
  and took that average from a full walk of the graph's footprint whenever the last walked figure
  was out of date: before the first walk, or once the element count had doubled or halved since
  it. Inside a unit that already holds changes (a script run as one unit, every saved-query call,
  a transaction) such a walk is not cached, so every following writing traversal of the unit
  walked the graph again: time proportional to the graph per traversal. A loader that dropped most
  of a 250 000-element graph and then ran some 200 000 single `merge_e(...)` traversals in scripts
  of 300 took over ten minutes instead of under one. Now the tracked measure (the `graphersal`
  command line, the dev server, the playground, Python) never walks the graph for this, and the
  estimated measure (a library host without the tracking allocator) never walks the graph inside
  a unit that holds changes, also near the ceiling: it keeps the last walked figure, adjusted by
  the elements added or removed, until the unit ends; between units a stale figure is walked
  again at most once, and cached, as before. In the estimated mode the figure inside such a unit
  is therefore an extrapolation from the last walk and can be off after large changes in it;
  install the tracking allocator for exact figures
  ([Resource Limits](../users_guide/resource_limits.md)).

## Upgrading from 0.5.0

A drop-in replacement: no format change, no API change, no upgrade step. Stores, snapshots,
journals and backups of 0.5.0 and 0.5.1 are interchangeable.
