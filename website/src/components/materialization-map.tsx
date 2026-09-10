const stages = [
  {
    index: "01",
    title: "Exact Git tree",
    detail: "a4d2c19",
  },
  {
    index: "02",
    title: "Immutable base",
    detail: "reuse or create",
  },
  {
    index: "03",
    title: "APFS clone",
    detail: "shared blocks",
  },
  {
    index: "04",
    title: "Linked worktree",
    detail: "clean + writable",
  },
] as const;

export function MaterializationMap() {
  return (
    <figure className="materialization-map graph-frame">
      <span className="corner corner-tl" aria-hidden="true">+</span>
      <span className="corner corner-tr" aria-hidden="true">+</span>
      <span className="corner corner-bl" aria-hidden="true">+</span>
      <span className="corner corner-br" aria-hidden="true">+</span>
      <figcaption className="frame-title">[ FROM TREE TO WORKSPACE ]</figcaption>

      <ol className="materialization-track" aria-label="Riftri worktree materialization path">
        {stages.map((stage) => (
          <li key={stage.index}>
            <span>{stage.index}</span>
            <strong>{stage.title}</strong>
            <small>{stage.detail}</small>
          </li>
        ))}
      </ol>

      <div className="materialization-foot">
        <span>GIT OWNS THE WORKTREE</span>
        <span><i aria-hidden="true" /> RIFTRI EXITS AFTER CREATION</span>
      </div>
    </figure>
  );
}
