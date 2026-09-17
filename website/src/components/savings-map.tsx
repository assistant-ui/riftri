import data from "../data/space-savings.json";
import { SavingsBackendName } from "./savings-backend-name";

const toMiB = (bytes: number) => (bytes / 2 ** 20).toFixed(2);
const savedPercent = (1 - data.riftriBytes / data.gitBytes) * 100;
const lanes = [
  { name: "Ordinary Git", optimized: false, bytes: data.gitBytes, detail: "10 full checkouts", className: "" },
  { name: "Riftri", optimized: true, bytes: data.riftriBytes, detail: "10 views + shared base", className: "savings-lane-riftri" },
];

export function SavingsMap() {
  return (
    <figure className="savings-map graph-frame" aria-describedby="savings-scope">
      <span className="corner corner-tl" aria-hidden="true">+</span>
      <span className="corner corner-tr" aria-hidden="true">+</span>
      <span className="corner corner-bl" aria-hidden="true">+</span>
      <span className="corner corner-br" aria-hidden="true">+</span>
      <figcaption className="frame-title">[ ASSISTANT-UI / DISK ALLOCATION ]</figcaption>
      <div className="savings-meta">
        <span>{data.worktrees} WORKTREES / {data.filesPerView.toLocaleString("en-US")} FILES EACH</span>
      </div>

      <div className="savings-comparison">
        <div className="savings-chart">
          <p className="savings-chart-label">APFS reference measurement · new volume allocation</p>
          <dl className="savings-lanes">
            {lanes.map((lane) => (
              <div className={`savings-lane ${lane.className}`} key={lane.name}>
                <dt>{lane.optimized ? <SavingsBackendName /> : lane.name}<small>{lane.detail}</small></dt>
                <dd>
                  <strong>{toMiB(lane.bytes)} <span>MiB</span></strong>
                  <span className="savings-bar" aria-hidden="true">
                    <span>[</span>
                    <span className="savings-track">
                      <span
                        className="savings-fill"
                        style={{ clipPath: `inset(0 ${(1 - lane.bytes / data.gitBytes) * 100}% 0 0)` }}
                      />
                    </span>
                    <span>]</span>
                  </span>
                </dd>
              </div>
            ))}
          </dl>
          <div className="savings-scale" aria-hidden="true">
            <span>0 MiB</span><span>{toMiB(data.gitBytes)} MiB</span>
          </div>
        </div>
        <div className="savings-result">
          <span className="savings-result-label">LESS NEW ALLOCATION</span>
          <strong>{savedPercent.toFixed(1)}<span>%</span></strong>
          <p>{toMiB(data.gitBytes - data.riftriBytes)} MiB saved</p>
          <small>Includes the shared base and all ten views.</small>
        </div>
      </div>

      <div className="savings-notes">
        <p id="savings-scope">
          Source files only on APFS. Dependencies and builds excluded.
        </p>
        <details className="savings-details">
          <summary>Benchmark details</summary>
          <p>
            Creation time: Riftri {data.riftriSeconds.toFixed(2)} s · Git {data.gitSeconds.toFixed(2)} s.
          </p>
          <p>
            <time dateTime={data.date}>12 Sep 2026</time> · adjusted assistant-ui snapshot
            {" "}(<code>linguist-generated</code> removed). Disk use measured at the volume level.
          </p>
          <p>
            Backend names show support, not Linux or Windows measurements. Results vary.
          </p>
          <a href={`https://github.com/assistant-ui/riftri/blob/main/${data.reportPath}`}>
            Read full benchmark <span aria-hidden="true">↗</span>
          </a>
        </details>
      </div>
    </figure>
  );
}
