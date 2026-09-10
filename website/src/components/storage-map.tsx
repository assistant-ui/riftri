const views = [
  { index: "01", name: "auth", branch: "feature/auth", changed: 3 },
  { index: "02", name: "billing", branch: "feature/billing", changed: 2 },
  { index: "03", name: "tests", branch: "fix/worktree-tests", changed: 1 },
] as const;

function BlockTrack({ changed }: { changed: number }) {
  return (
    <span className="block-track" aria-hidden="true">
      {Array.from({ length: 16 }, (_, index) => (
        <span className={index >= 16 - changed ? "private-block" : "shared-block"} key={index}>
          {index >= 16 - changed ? "█" : "·"}
        </span>
      ))}
    </span>
  );
}

export function StorageMap() {
  return (
    <figure className="storage-map graph-frame">
      <span className="corner corner-tl" aria-hidden="true">+</span>
      <span className="corner corner-tr" aria-hidden="true">+</span>
      <span className="corner corner-bl" aria-hidden="true">+</span>
      <span className="corner corner-br" aria-hidden="true">+</span>
      <figcaption className="frame-title">[ LIVE WORKTREE MAP ]</figcaption>
      <div className="map-head">
        <span>REPOSITORY / EXACT TREE</span>
        <span>a4d2c19</span>
      </div>
      <div className="base-node">
        <span className="node-index">00</span>
        <div>
          <strong>immutable base</strong>
          <small>one materialized Git tree / read only</small>
        </div>
        <span className="base-track" aria-hidden="true">████████████████</span>
      </div>
      <div className="branch-line" aria-hidden="true">└──────────────┬──────────────┐</div>
      <div className="view-list">
        {views.map((view) => (
          <div className="view-row" key={view.name}>
            <span className="node-index">{view.index}</span>
            <div>
              <strong>{view.name}/</strong>
              <small>{view.branch}</small>
            </div>
            <BlockTrack changed={view.changed} />
          </div>
        ))}
      </div>
      <div className="map-legend">
        <span><i>·</i> shared block</span>
        <span><i>█</i> private edit</span>
      </div>
    </figure>
  );
}
