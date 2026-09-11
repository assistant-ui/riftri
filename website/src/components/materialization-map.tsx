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
    title: "Native COW view",
    detail: "platform backend",
  },
  {
    index: "04",
    title: "Linked worktree",
    detail: "clean + writable",
  },
] as const;

const platforms = [
  {
    name: "macOS",
    backend: "APFS clone",
    status: "current",
  },
  {
    name: "Linux",
    backend: "reflink / OverlayFS",
    status: "planned · M5",
  },
  {
    name: "Windows",
    backend: "ReFS block clone",
    status: "planned · M7",
  },
] as const;

function BackendCycle() {
  return (
    <>
      <span className="backend-cycle" aria-hidden="true">
        {platforms.map((platform) => (
          <span className="backend-cycle-item" key={platform.name}>
            <strong>{platform.backend}</strong>
            <small>{platform.name} · {platform.status}</small>
          </span>
        ))}
      </span>
      <span className="visually-hidden">
        Native copy-on-write view. APFS clone on macOS is current. Linux reflink or OverlayFS is
        planned for milestone 5. Windows ReFS block clone is planned for milestone 7.
      </span>
    </>
  );
}

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
          <li className={stage.index === "03" ? "is-backend-stage" : undefined} key={stage.index}>
            <span>{stage.index}</span>
            {stage.index === "03" ? (
              <BackendCycle />
            ) : (
              <>
                <strong>{stage.title}</strong>
                <small>{stage.detail}</small>
              </>
            )}
          </li>
        ))}
      </ol>

      <ul className="platform-lane" aria-label="Riftri native storage backend roadmap">
        {platforms.map((platform) => (
          <li
            className={
              platform.status === "current" ? "platform-backend is-current" : "platform-backend"
            }
            key={platform.name}
          >
            <span className="platform-signal" aria-hidden="true" />
            <div>
              <strong>{platform.name}</strong>
              <small>{platform.backend}</small>
            </div>
            <span className="platform-status">{platform.status}</span>
          </li>
        ))}
      </ul>

      <div className="materialization-foot">
        <span>GIT OWNS THE WORKTREE</span>
        <span><i aria-hidden="true" /> RIFTRI EXITS AFTER CREATION</span>
      </div>
    </figure>
  );
}
