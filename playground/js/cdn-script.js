// Loads a pinned classic script from a CDN on first use, with Subresource Integrity: the browser
// refuses the file when its hash differs from `integrity` (the CDN or the network changed it).
// Used by the libraries the playground loads lazily (sigma.js + graphology for the Fast tier,
// mermaid for the Schema panel's Mermaid tab); the eagerly loaded ones are in index.html.

/**
 * Appends `<script src integrity crossorigin="anonymous" referrerpolicy="no-referrer">` and
 * resolves when it ran; rejects (and removes the element) when it could not be loaded.
 * @param {{src: string, integrity: string}} script
 */
export function loadScript({ src, integrity }) {
  return new Promise((resolve, reject) => {
    const el = document.createElement('script');
    el.src = src;
    el.integrity = integrity;
    el.crossOrigin = 'anonymous';
    el.referrerPolicy = 'no-referrer';
    el.onload = () => resolve();
    el.onerror = () => {
      el.remove();
      reject(new Error(`could not load ${src} (offline, or blocked)`));
    };
    document.head.appendChild(el);
  });
}
