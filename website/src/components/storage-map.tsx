const views = [
  { label: "auth", branch: "feature/auth", changed: 4 },
  { label: "billing", branch: "feature/billing", changed: 2 },
  { label: "tests", branch: "fix/worktree-tests", changed: 3 },
] as const;

function Blocks({ changed }: { changed: number }) {
  return (
    <span className="block-row" aria-hidden="true">
      {Array.from({ length: 18 }, (_, index) => (
        <span className={index >= 18 - changed ? "block block-changed" : "block"} key={index} />
      ))}
    </span>
  );
}

export function StorageMap() {
  return (
    <figure className="storage-map">
      <figcaption>
        <span>[ SHARED TREE ]</span>
        <span>APFS / COW</span>
      </figcaption>

      <div className="base-row">
        <span className="map-index">00</span>
        <div>
          <strong>immutable base</strong>
          <span>exact Git tree · read only</span>
        </div>
        <span className="base-blocks" aria-hidden="true">
          ██████████████████
        </span>
      </div>

      <div className="fork-line" aria-hidden="true">
        ├──────────┬──────────┤
      </div>

      <div className="view-list">
        {views.map((view, index) => (
          <div className="view-row" key={view.label}>
            <span className="map-index">0{index + 1}</span>
            <div className="view-meta">
              <strong>{view.label}</strong>
              <span>{view.branch}</span>
            </div>
            <Blocks changed={view.changed} />
          </div>
        ))}
      </div>

      <div className="map-legend">
        <span>
          <i className="legend-block" /> shared
        </span>
        <span>
          <i className="legend-block legend-block-changed" /> private change
        </span>
      </div>
    </figure>
  );
}
