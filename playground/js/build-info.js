// The version shown in the header: the engine version (`info().version`) and, for the WebAssembly
// playground, the commit its module was built from (`pkg/build-info.json`, written by
// playground/build.sh). The dev server has no build file: it shows the engine version only. A UI
// profile with a `version` (a product built on the engine) shows the product's version instead,
// and the engine's in the tooltip.

const BUILD_INFO_URL = new URL('../pkg/build-info.json', import.meta.url);

/**
 * The build file, or null when there is none (an older build, the dev server) or it is unreadable.
 * @returns {Promise<{commit: string, tag: string, dirty: boolean}|null>}
 */
export async function loadBuildInfo() {
  try {
    const r = await fetch(BUILD_INFO_URL, { cache: 'no-store' });
    if (!r.ok) return null;
    const data = await r.json();
    return data && typeof data.commit === 'string' ? data : null;
  } catch {
    return null;
  }
}

/**
 * The header's text and tooltip. A clean build of the release tag `v<version>` shows the version
 * alone (`v0.2.0`); any other build adds its commit (`v0.2.0 · bc7114c`) and `+ changes` when the
 * tree had uncommitted changes.
 * @param {string|null|undefined} version  the engine version, e.g. "0.2.0"
 * @param {{commit?: string, tag?: string, dirty?: boolean}|null|undefined} build
 * @param {{title?: string, version?: string|null}|null} [product]  the UI profile: with a
 *   `version`, the label shows `v<product version>` and the tooltip names the product and the
 *   engine (`graphersal-memory 0.1.0 · Graphersal engine 0.6.1`)
 * @returns {{text: string, title: string}|null}  null without a version
 */
export function versionLabel(version, build, product = null) {
  if (product && product.version) {
    const engine = version ? ` · Graphersal engine ${version}` : '';
    return { text: `v${product.version}`, title: `${product.title || 'Version'} ${product.version}${engine}` };
  }
  if (!version) return null;
  const release = `v${version}`;
  const commit = build && build.commit ? build.commit : '';
  const dirty = !!(build && build.dirty);
  const isRelease = !!build && build.tag === release && !dirty;
  let text = release;
  if (commit && !isRelease) text += ` · ${commit}${dirty ? ' + changes' : ''}`;
  let title = `Engine ${version}`;
  if (isRelease) title += `, the release build (commit ${commit})`;
  else if (commit) title += `, a development build of commit ${commit}${dirty ? ' with uncommitted changes' : ''}, not a release`;
  return { text, title };
}
