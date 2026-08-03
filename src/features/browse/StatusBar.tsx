import { useStore } from "../../state/store";
import { fmtCount } from "../../lib/format";

export function StatusBar() {
  const total = useStore((s) => s.total);
  const queryMs = useStore((s) => s.queryMs);

  return (
    <div className="flex items-center gap-4 border-t border-neutral-800 bg-neutral-950 px-3 py-1 text-xs text-neutral-500">
      <span>{fmtCount(total)} files</span>
      {queryMs != null && <span>query {queryMs.toFixed(0)} ms</span>}
    </div>
  );
}
