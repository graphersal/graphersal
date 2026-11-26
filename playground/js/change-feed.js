// The dev server's change feed (`GET /api/changes`), polled while the page is visible: when the
// graph changed because of something THIS page did not do (another browser tab, an AI agent over
// MCP), the page reloads its graph, schema, statistics and Store menu and marks the tabs' results
// as possibly stale. Pure logic plus a small poller with injectable timers, so both are tested
// under Node (playground/tests/change-feed.test.mjs).
//
// The server answers `{version, commitSeq, recent: [{version, by, commitSeq}], view, mcp}`:
// `version` grows with every change of the served graph (a commit, a view opened or closed, a
// rollback), `recent` names who made the last ones (`by`: the `X-Graphersal-Client` id of a page,
// `mcp`, or empty; `"another identity"` for a change made under another identity than this
// page's when the host attaches identities, which is never this page's id).

/** Poll interval while the page is visible. */
export const POLL_MS = 2000;

/**
 * What a feed answer means for a page that knows `knownVersion` and sends `clientId`.
 * `external` is true when at least one change after `knownVersion` was not made by this page,
 * also when the feed no longer lists all of them (a gap: somebody changed a lot).
 * @param {{version:number, commitSeq:number, recent?:Array<{version:number, by:string}>}} answer
 * @param {number|null} knownVersion  null: the first answer (sets the baseline, never external)
 * @param {string} clientId
 * @returns {{version:number, commitSeq:number, changed:boolean, external:boolean}}
 */
export function readChanges(answer, knownVersion, clientId) {
  const version = Number(answer && answer.version) || 0;
  const commitSeq = Number(answer && answer.commitSeq) || 0;
  if (knownVersion === null || knownVersion === undefined || version <= knownVersion) {
    // A restarted server counts from 0 again: a smaller version is a new baseline.
    const restarted = knownVersion !== null && knownVersion !== undefined && version < knownVersion;
    return { version, commitSeq, changed: restarted, external: restarted };
  }
  const recent = Array.isArray(answer.recent) ? answer.recent : [];
  const newer = recent.filter((c) => Number(c.version) > knownVersion);
  const covered = newer.length === version - knownVersion;
  const external = !covered || newer.some((c) => c.by !== clientId);
  return { version, commitSeq, changed: true, external };
}

/** A random id for this page (one per page load; each browser tab is its own client). */
export function newClientId() {
  const c = globalThis.crypto;
  if (c && typeof c.randomUUID === 'function') return `tab-${c.randomUUID()}`;
  return `tab-${Math.random().toString(36).slice(2)}${Date.now().toString(36)}`;
}

/**
 * Polls `fetchChanges()` every `intervalMs` while `isVisible()`; every answer goes to
 * `onAnswer(answer)` (awaited, so a slow reload never overlaps the next poll). A failed poll is
 * ignored (the server may be restarting). `kick()` polls now (the page became visible again).
 * @param {{fetchChanges: () => Promise<object>, onAnswer: (answer: object) => Promise<void>|void,
 *   isVisible?: () => boolean, intervalMs?: number, setTimer?: Function, clearTimer?: Function}} o
 */
export function createPoller(o) {
  const setTimer = o.setTimer || ((fn, ms) => setTimeout(fn, ms));
  const clearTimer = o.clearTimer || ((t) => clearTimeout(t));
  const isVisible = o.isVisible || (() => true);
  const intervalMs = o.intervalMs || POLL_MS;
  let timer = null;
  let running = false;
  let busy = false;

  async function tick() {
    timer = null;
    if (!running) return;
    if (isVisible() && !busy) {
      busy = true;
      try {
        await o.onAnswer(await o.fetchChanges());
      } catch (err) {
        /* not reachable right now: try again next time */
      } finally {
        busy = false;
      }
    }
    if (running && timer === null) timer = setTimer(tick, intervalMs);
  }

  return {
    start() {
      if (running) return;
      running = true;
      timer = setTimer(tick, 0);
    },
    stop() {
      running = false;
      if (timer !== null) clearTimer(timer);
      timer = null;
    },
    kick() {
      if (!running || busy) return;
      if (timer !== null) clearTimer(timer);
      timer = setTimer(tick, 0);
    },
    get running() {
      return running;
    },
  };
}

/**
 * The MCP endpoint's address: the server's `mcp.endpoint` (`/api/info`, `/api/changes`; already
 * prefixed by the mount of a multi-database server, e.g. `/db/acme/mcp`) on this page's host.
 */
export function mcpEndpointUrl(mcp, host, protocol = 'http:') {
  if (!mcp) return '';
  const scheme = protocol === 'https:' ? 'https:' : 'http:';
  return `${scheme}//${host}${mcp.endpoint || '/mcp'}`;
}

/** The tooltip of the top bar's MCP badge. */
export function mcpBadgeTitle(mcp, host, protocol = 'http:') {
  if (!mcp) return '';
  const where = `MCP endpoint ${mcpEndpointUrl(mcp, host, protocol)}`;
  const mode = mcp.readOnly ? ' (read-only for agents)' : '';
  const client = mcp.client
    ? `; connected: ${mcp.client.name}${mcp.client.version ? ' ' + mcp.client.version : ''}`
    : '; no agent connected yet';
  return `${where}${mode}${client}. An AI agent works on this same graph; this page reloads when it changes the graph.`;
}
