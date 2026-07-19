import { useEffect, useState } from 'react';
import { Badge, Button, Callout, Card, Icon } from '../../design';
import { injectCss } from '../../design/injectCss';
import { ApiError, getLeaderboard, type Leaderboard, type LeaderboardEntry } from '../../api/runs';
import { NotPaperConfirmedCallout } from './VintageInspector';

const CSS = `
.qe-lb { max-width: var(--content-max); margin: 0 auto; padding: 18px; display: flex; flex-direction: column; gap: 16px; }
.qe-lb__hd { display: flex; align-items: center; justify-content: space-between; gap: 16px; }
.qe-lb__hd h2 { font-family: var(--font-display); font-size: var(--fs-lg); font-weight: 600; }
.qe-lb__sub { font-size: var(--fs-sm); color: var(--text-tertiary); margin-top: 2px; max-width: 70ch; }
.qe-lb__diag { display: grid; grid-template-columns: repeat(3, 1fr); gap: 1px; background: var(--border-subtle); border: 1px solid var(--border-subtle); border-radius: var(--radius-md); overflow: hidden; }
.qe-lb__diag .m { background: var(--surface-card); padding: 12px 14px; display: flex; flex-direction: column; gap: 3px; }
.qe-lb__diag .m .k { font: 500 10px var(--font-mono); text-transform: uppercase; letter-spacing: .06em; color: var(--text-muted); }
.qe-lb__diag .m .v { font-family: var(--font-mono); font-variant-numeric: tabular-nums; font-size: 15px; font-weight: 600; color: var(--text-primary); }
.qe-lb__diag .m .note { font: 500 10px var(--font-mono); color: var(--text-tertiary); }
.qe-lb__note { font-size: 12px; color: var(--text-tertiary); margin-top: 8px; }
.qe-lb__table { width: 100%; overflow-x: auto; }
.qe-lb__table table { width: 100%; border-collapse: collapse; }
.qe-lb__table th { text-align: left; padding: 8px 10px; font: 500 10px var(--font-mono); text-transform: uppercase; letter-spacing: .06em; color: var(--text-muted); border-bottom: 1px solid var(--border-subtle); }
.qe-lb__table th.num, .qe-lb__table td.num { text-align: right; font-variant-numeric: tabular-nums; }
.qe-lb__table td { padding: 10px; border-bottom: 1px solid var(--border-subtle); font-family: var(--font-mono); font-size: 12px; color: var(--text-secondary); vertical-align: top; }
.qe-lb__table td .lead { font-size: 14px; font-weight: 600; color: var(--text-primary); }
.qe-lb__rank { font-weight: 600; color: var(--text-primary); }
.qe-lb__id { color: var(--text-primary); }
.qe-lb__row--escalated td { background: var(--surface-inset); color: var(--text-muted); }
.qe-lb__row--escalated td .lead { color: var(--text-muted); }
.qe-lb__dsr { display: inline-flex; align-items: center; gap: 6px; }
.qe-lb__steer { display: flex; flex-wrap: wrap; gap: 4px; color: var(--text-tertiary); }
.qe-lb__empty { padding: 40px 24px; text-align: center; color: var(--text-tertiary); font-size: var(--fs-sm); }
`;

injectCss('qe-lb-css', CSS);

/** Format a float verbatim (no recomputation — presentation only). */
function num(v: number | null | undefined, dp = 3): string {
  if (v == null || Number.isNaN(v)) return '—';
  return v.toFixed(dp);
}

/** Format a USD capacity figure. */
function usd(v: number | null | undefined): string {
  if (v == null || Number.isNaN(v)) return '—';
  return `$${Math.round(v).toLocaleString('en-US')}`;
}

const PROV_VARIANT: Record<string, 'neutral' | 'warn'> = {
  real: 'neutral',
  synthetic: 'warn',
  mixed: 'warn',
};

/** The DSR bar cell — escalated/greyed (with a warning) for over-consulted vintages (QE-466 enforcement). */
function DsrBar({ entry }: { entry: LeaderboardEntry }) {
  if (entry.dsr_status === 'escalated') {
    return (
      <span className="qe-lb__dsr" title="Holdout over-consulted — DSR bar escalated; vintage demoted.">
        <Icon name="octagon-x" size={13} />
        <span>{num(entry.dsr)}</span>
        <Badge variant="warn">ESCALATED</Badge>
      </span>
    );
  }
  return <span className="qe-lb__dsr">{num(entry.dsr)}</span>;
}

/** The per-vintage steer/param diff (indicator subset + budget + windows/folds). */
function SteerDiff({ entry }: { entry: LeaderboardEntry }) {
  const s = entry.steer_delta;
  if (!s) return <span className="qe-lb__steer">—</span>;
  return (
    <span className="qe-lb__steer">
      <span title={`indicator subset ${s.indicator_subset_hash}`}>subset {s.indicator_subset_hash.slice(0, 8)}…</span>
      <span>· gens {s.generations}</span>
      <span>· pop {s.population}</span>
      <span>· win {s.windows}</span>
      <span>· folds {s.folds}</span>
    </span>
  );
}

export interface VintageLeaderboardProps {
  /** Open a vintage in the read-only inspector (the ONLY navigation the surface offers — no promote/select). */
  onOpen: (vintageId: string) => void;
  /** Return to the vintage browser. */
  onBack: () => void;
}

/**
 * VintageLeaderboard (QE-466) — the read-only leaderboard/comparison over already-sealed vintages. It ranks on
 * the **persisted, net-of-cost** evidence `GET /api/vintages/leaderboard` reads from the sealed artefacts
 * (never recomputed): the deployed capacity-capped cost-stress net leads, with capacity-at-size and realised
 * turnover; the QE-430-deflated cross-vintage correlation + effective N are shown as a **diversity
 * diagnostic** (are these diverse, or the same bet re-drawn?). Over-consulted vintages have their DSR bar
 * escalated/greyed and are demoted (the consultation budget is ENFORCED, not just displayed).
 *
 * **Structurally not a selector.** The surface exposes NO promote / select-best / auto-run affordance — the
 * only action a row offers is opening the read-only inspector. Every vintage is labelled "backtest-holdout
 * only — not paper-confirmed", and a standing caveat states that re-running until the top slot improves is the
 * rejected best-of-N pattern. Promotion stays through the existing per-run G1 gate + seal.
 */
export function VintageLeaderboard({ onOpen, onBack }: VintageLeaderboardProps) {
  const [board, setBoard] = useState<Leaderboard | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    getLeaderboard()
      .then((b) => {
        if (!cancelled) setBoard(b);
      })
      .catch((e) => {
        if (!cancelled) setError(e instanceof ApiError ? e.message : 'Failed to load the leaderboard.');
      });
    return () => {
      cancelled = true;
    };
  }, []);

  const back = (
    <Button variant="ghost" size="sm" onClick={onBack} iconLeft={<Icon name="arrow-left" size={15} />}>
      All vintages
    </Button>
  );

  return (
    <div className="qe-lb">
      <div className="qe-lb__hd">
        <div>
          <h2>Vintage leaderboard</h2>
          <div className="qe-lb__sub">
            Sealed vintages ranked on their persisted net-of-cost holdout evidence (deployed capacity-capped
            weights) — inspection only, never a selector.
          </div>
        </div>
        {back}
      </div>

      <NotPaperConfirmedCallout />

      {board && (
        <Callout variant="warn" title="Cross-vintage ranking is inspection — not selection">
          {board.caveat}
        </Callout>
      )}

      {error && (
        <Callout variant="danger" title="Could not load the leaderboard">
          {error}
        </Callout>
      )}

      {board && (
        <Card title="Cross-vintage diversity (QE-430 R(N)/Fisher-z — a diagnostic, never a rank input)">
          <div className="qe-lb__diag">
            <div className="m">
              <span className="k">Deflated correlation</span>
              <span className="v">{num(board.cross_vintage_correlation)}</span>
              <span className="note">same bet re-drawn ⇒ high</span>
            </div>
            <div className="m">
              <span className="k">Effective N</span>
              <span className="v">{board.effective_n}</span>
              <span className="note">aligned series length</span>
            </div>
            <div className="m">
              <span className="k">Enforcement</span>
              <span className="v" style={{ fontSize: 12 }}>
                {board.enforcement_posture}
              </span>
              <span className="note">budget {board.consultation_budget} · posture (b)</span>
            </div>
          </div>
          <div className="qe-lb__note">{board.effective_n_note}</div>
        </Card>
      )}

      <Card title="Ranked vintages (persisted net-of-cost leads)">
        {board == null && !error && <div className="qe-lb__empty">Loading leaderboard…</div>}
        {board != null && board.entries.length === 0 && (
          <div className="qe-lb__empty">No sealed vintages yet. Run a train campaign to produce one.</div>
        )}
        {board != null && board.entries.length > 0 && (
          <div className="qe-lb__table">
            <table>
              <thead>
                <tr>
                  <th className="num">#</th>
                  <th>Vintage</th>
                  <th>Provenance</th>
                  <th className="num">Net-of-cost min&#123;1×,2×&#125;</th>
                  <th className="num">Capacity</th>
                  <th className="num">Turnover</th>
                  <th>DSR</th>
                  <th className="num">Consultations</th>
                  <th>Steer / params</th>
                </tr>
              </thead>
              <tbody>
                {board.entries.map((e) => (
                  <tr
                    key={e.id}
                    className={
                      e.over_consulted
                        ? 'qe-lb__row--escalated qe-table__row--clickable'
                        : 'qe-table__row--clickable'
                    }
                    role="button"
                    tabIndex={0}
                    onClick={() => onOpen(e.id)}
                    onKeyDown={(ev) => {
                      if (ev.key === 'Enter' || ev.key === ' ') {
                        ev.preventDefault();
                        onOpen(e.id);
                      }
                    }}
                    aria-label={`Inspect vintage ${e.id}${e.over_consulted ? ' (holdout over-consulted)' : ''}`}
                  >
                    <td className="num qe-lb__rank">{e.rank}</td>
                    <td>
                      <span className="qe-lb__id" title={e.content_hash}>
                        {e.id}
                      </span>
                    </td>
                    <td>
                      <Badge variant={PROV_VARIANT[e.data_provenance] ?? 'warn'}>
                        {e.data_provenance.toUpperCase()}
                      </Badge>
                    </td>
                    <td className="num">
                      <span className="lead">{num(e.cost_stress_net_min)}</span>
                    </td>
                    <td className="num">{usd(e.capacity_usd)}</td>
                    <td className="num">{num(e.realised_turnover, 4)}</td>
                    <td>
                      <DsrBar entry={e} />
                    </td>
                    <td className="num">
                      {e.consultation_count}
                      {e.over_consulted ? ' ⚠' : ''}
                    </td>
                    <td>
                      <SteerDiff entry={e} />
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}
      </Card>
    </div>
  );
}
