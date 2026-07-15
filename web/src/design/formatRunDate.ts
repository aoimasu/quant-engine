/**
 * Format an epoch-ms timestamp as a compact UTC `YYYY-MM-DD HH:MM` string (QE-410). Promoted from the
 * identical `fmtDate` helpers copied into `BacktestsList` and `TrainingList` so the run lists format
 * dates from one place.
 */
export function formatRunDate(ms: number): string {
  const d = new Date(ms);
  const y = d.getUTCFullYear();
  const m = String(d.getUTCMonth() + 1).padStart(2, '0');
  const day = String(d.getUTCDate()).padStart(2, '0');
  const hh = String(d.getUTCHours()).padStart(2, '0');
  const mm = String(d.getUTCMinutes()).padStart(2, '0');
  return `${y}-${m}-${day} ${hh}:${mm}`;
}
