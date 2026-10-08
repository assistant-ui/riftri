import { MaterializationTrack } from "./materialization-track";

export function MaterializationMap() {
  return (
    <figure className="materialization-map graph-frame">
      <span className="corner corner-tl" aria-hidden="true">+</span>
      <span className="corner corner-tr" aria-hidden="true">+</span>
      <span className="corner corner-bl" aria-hidden="true">+</span>
      <span className="corner corner-br" aria-hidden="true">+</span>
      <figcaption className="frame-title">[ FROM TREE TO WORKSPACE ]</figcaption>

      <MaterializationTrack />

      <div className="materialization-foot">
        <span>GIT OWNS THE WORKTREE</span>
        <span><i aria-hidden="true" /> RIFTRI EXITS AFTER CREATION</span>
      </div>
    </figure>
  );
}
