// The schema editor's flow: draft -> "What changes" (diff_schema) -> "Check against data"
// (validate_schema_patch) -> "Apply" (patch_schema), and the one-click "Freeze as open/closed"
// (set_schema(infer_schema(), mode)). Pure logic over a `SchemaApi` (api.js) and a
// `SchemaDraft` (draft.js); no Vue, no DOM (tested under Node with a fake api). The component
// wraps an instance in `reactive()`.

import { SchemaDraft } from './draft.js';

const message = (err) => (err && err.message ? err.message : String(err));

/**
 * The engine's fill warning as HTML: escaped, the element count and the time estimate in bold,
 * one paragraph line per sentence line.
 */
export function fillHtml(text) {
  const escaped = String(text || '')
    .replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;').replace(/"/g, '&quot;');
  return escaped
    .replace(/(up to )?\d[\d,]* elements?/g, '<b>$&</b>')
    .replace(/\((about [^)]*|under a second|time depends on the model)\)/g, '(<b>$1</b>)')
    .replace(/\n/g, '<br>');
}

export class SchemaFlow {
  /** @param {import('./api.js').SchemaApi} api */
  constructor(api) {
    this.api = api;
    this.ed = new SchemaDraft();
    this.loaded = false;
    this.loadError = null;
    this.busy = null; // 'load' | 'diff' | 'check' | 'apply' | 'infer' | 'freeze' | null
    this.diff = null; // {version, changes, compatible}
    this.diffError = null; // {version, message}
    this.check = null; // {version, violations, diff, compatible, via: 'patch'|'schema'}
    this.checkError = null; // {version, message}
    this.applyError = null;
    this.freezeError = null;
    // A vector fill the draft starts (book "Vector Rules"): above the threshold Apply stops
    // here and the dialog asks ({version, warning, fill}); `applyAndLock()` goes on.
    this.pendingFill = null;
    // The warning text of the last applied fill below the threshold (a plain note).
    this.fillNote = null;
    // The graph changed after the schema shown was inferred (or the stored one was dropped)
    // and nothing inferred it again: "may be outdated - refresh". Inference is never automatic
    // on a change of the graph (it walks the whole graph: ~0.7 s on `large`).
    this.outdated = false;
  }

  /**
   * Reads the stored schema (`get_schema`: cheap and authoritative). Without one, `infer: true`
   * (the default: the panel's first show, its refresh button) infers it (`infer_schema`), while
   * `infer: false` (a change of the graph) keeps the schema inferred last and marks it
   * `outdated`. A stored schema clears `outdated`.
   * @param {{infer?: boolean}} [opts]
   */
  async load({ infer = true } = {}) {
    this.busy = this.busy || 'load';
    try {
      const stored = await this.api.get();
      if (stored) {
        this.ed.setEngineState({ stored, inferred: null });
        this.outdated = false;
      } else if (infer) {
        this.ed.setEngineState({ stored: null, inferred: await this.api.infer() });
        this.outdated = false;
      } else {
        this.ed.setEngineState({ stored: null, inferred: this.ed.inferred });
        this.outdated = true;
      }
      this.loadError = null;
      // The draft stays, but what it changes and how the data fares were measured against
      // the old state.
      this.diff = null;
      this.diffError = null;
      this.check = null;
      this.checkError = null;
    } catch (err) {
      this.loadError = message(err);
    } finally {
      this.loaded = true;
      if (this.busy === 'load') this.busy = null;
    }
  }

  /** The diff result of the current draft version, or null. */
  get freshDiff() {
    return this.diff && this.diff.version === this.ed.version ? this.diff : null;
  }

  get freshDiffError() {
    return this.diffError && this.diffError.version === this.ed.version ? this.diffError.message : null;
  }

  /** The check result of the current draft version, or null. */
  get freshCheck() {
    return this.check && this.check.version === this.ed.version ? this.check : null;
  }

  get freshCheckError() {
    return this.checkError && this.checkError.version === this.ed.version ? this.checkError.message : null;
  }

  /** The current check found violations: Apply is blocked until the data or the draft changes. */
  get blocked() {
    const c = this.freshCheck;
    return !!(c && c.violations && c.violations.total_violations > 0);
  }

  /** Apply is possible: a draft that differs from the stored schema, no known violations, idle. */
  get canApply() {
    return this.ed.dirty && !this.blocked && !this.busy && !this.ed.jsonError;
  }

  /** "What changes": `diff_schema(draft)`. */
  async whatChanges() {
    if (!this.ed.draft) return;
    const version = this.ed.version;
    this.busy = 'diff';
    try {
      const res = await this.api.diff(this.ed.draft);
      this.diff = { version, changes: res.changes || [], compatible: !!res.compatible };
      this.diffError = null;
    } catch (err) {
      this.diffError = { version, message: message(err) };
    } finally {
      this.busy = null;
    }
  }

  /**
   * "Check against data": `validate_schema_patch(patch)`, or `validate_schema(draft)` when no
   * merge patch can express the draft (a member set to `null`). Also records the diff it returns.
   */
  async runCheck() {
    if (!this.ed.draft) return null;
    const version = this.ed.version;
    const { patch, exact } = this.ed.patch;
    const via = exact ? 'patch' : 'schema';
    const res = exact ? await this.api.validatePatch(patch) : await this.api.validate(this.ed.draft);
    this.check = {
      version, via, violations: res.violations, diff: res.diff || [], compatible: !!res.compatible,
      fill: res.fill || [], fillWarning: res.fill_warning || null,
    };
    this.checkError = null;
    this.diff = { version, changes: res.diff || [], compatible: !!res.compatible };
    this.diffError = null;
    return this.check;
  }

  async checkAgainstData() {
    if (!this.ed.draft) return;
    const version = this.ed.version;
    this.busy = 'check';
    try {
      await this.runCheck();
    } catch (err) {
      this.checkError = { version, message: message(err) };
    } finally {
      this.busy = null;
    }
  }

  /**
   * "Apply": checks first (unless the current version was checked), stops on violations, else
   * `patch_schema(patch)` (one unit), or `set_schema(draft)` when the patch is not exact. On
   * success the draft is gone and the stored schema reloaded. Resolves to true when applied.
   *
   * A draft that fills vectors (a vector rule added or changed) locks the whole database while
   * the fill runs: when a planned fill `needs_confirmation`, Apply stops with `pendingFill`
   * (the dialog "Apply and lock" / "Cancel" shows its warning, the engine's one text) unless
   * `confirmed`; below the threshold it applies and keeps the text as `fillNote`.
   * @param {{confirmed?: boolean}} [opts]
   */
  async apply({ confirmed = false } = {}) {
    if (!this.ed.draft || this.busy) return false;
    const version = this.ed.version;
    this.busy = 'apply';
    this.applyError = null;
    this.fillNote = null;
    let stage = 'check';
    try {
      const check = this.freshCheck || (await this.runCheck());
      if (check.violations && check.violations.total_violations > 0) return false;
      const fill = check.fill || [];
      if (!confirmed && fill.some((f) => f.needs_confirmation)) {
        this.pendingFill = { version, warning: check.fillWarning, fill };
        return false;
      }
      this.pendingFill = null;
      const note = fill.length ? check.fillWarning : null;
      stage = 'apply';
      const { patch, exact } = this.ed.patch;
      if (exact) await this.api.patch(patch);
      else await this.api.set(this.ed.draft);
      this.ed.discard();
      this.diff = null;
      this.check = null;
      this.fillNote = note;
      await this.load({ infer: false }); // no inference: a dropped schema shows `outdated`
      return true;
    } catch (err) {
      if (stage === 'check') this.checkError = { version, message: message(err) };
      else this.applyError = message(err);
      return false;
    } finally {
      this.busy = null;
    }
  }

  /** "Apply and lock" in the fill dialog: applies the draft it was asked for. */
  async applyAndLock() {
    const pending = this.pendingFill;
    if (!pending || pending.version !== this.ed.version) {
      this.pendingFill = null;
      return false;
    }
    return this.apply({ confirmed: true });
  }

  /** "Cancel" in the fill dialog: nothing is applied. */
  cancelFill() {
    this.pendingFill = null;
  }

  /** "Infer from data": a new draft from `infer_schema()` (stores nothing). */
  async inferDraft() {
    this.busy = 'infer';
    try {
      this.ed.start(await this.api.infer(), 'inferred');
      this.applyError = null;
    } catch (err) {
      this.applyError = message(err);
    } finally {
      this.busy = null;
    }
  }

  /**
   * "Freeze as open/closed" is offered while the graph stores no enforcing schema (none, or mode
   * `none`) and no draft is being edited.
   */
  get canFreeze() {
    const stored = this.ed.stored;
    return !this.busy && !this.ed.dirty && (!stored || !stored.mode || stored.mode === 'none');
  }

  /**
   * "Freeze as open/closed": `set_schema(infer_schema(), mode)`, the structure the data has, in
   * one step. It always applies to the data it was inferred from, except in `closed` mode with
   * vertices or edges without a label (nothing declares them): then nothing changes and
   * `freezeError` holds the engine's report. Resolves to true when stored.
   * @param {'open'|'closed'} mode
   */
  async freeze(mode) {
    if (this.busy) return false;
    this.busy = 'freeze';
    this.freezeError = null;
    this.applyError = null;
    try {
      await this.api.set(await this.api.infer(), mode);
      this.ed.discard();
      this.diff = null;
      this.check = null;
      await this.load({ infer: false }); // no inference: a dropped schema shows `outdated`
      return true;
    } catch (err) {
      this.freezeError = message(err);
      return false;
    } finally {
      this.busy = null;
    }
  }

  discard() {
    this.ed.discard();
    this.pendingFill = null;
    this.applyError = null;
    this.freezeError = null;
  }
}
