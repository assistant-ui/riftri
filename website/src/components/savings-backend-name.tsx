const backends = ["APFS", "Linux reflink", "ReFS"];

export function SavingsBackendName() {
  return (
    <span className="savings-backend-label">
      Riftri /{" "}
      <span className="savings-backend-cycle" aria-hidden="true">
        {backends.map((backend) => <span className="savings-backend-item" key={backend}>{backend}</span>)}
      </span>
      <span className="visually-hidden">
        APFS on macOS, native reflinks on Linux, and ReFS on Windows.
        The chart shows the APFS reference measurement.
      </span>
    </span>
  );
}
