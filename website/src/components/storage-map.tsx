import { StorageFigure } from "./storage-figure";

export function StorageMap() {
  return (
    <figure className="storage-map graph-frame">
      <span className="corner corner-tl" aria-hidden="true">+</span>
      <span className="corner corner-tr" aria-hidden="true">+</span>
      <span className="corner corner-bl" aria-hidden="true">+</span>
      <span className="corner corner-br" aria-hidden="true">+</span>
      <figcaption className="frame-title">[ WORKTREE EXAMPLE ]</figcaption>
      <div className="map-head">
        <span>REPOSITORY / EXACT TREE</span>
        <span>a4d2c19</span>
      </div>
      <p className="visually-hidden">
        One immutable base holds the exact Git tree once. Three worktrees, auth, billing, and
        tests, are copy-on-write views over it. Each edit copies only the changed block into
        its worktree: three blocks in auth, two in billing, and one in tests. Every other block
        stays shared with the base.
      </p>
      <StorageFigure />
    </figure>
  );
}
