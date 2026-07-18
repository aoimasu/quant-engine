import { useCallback, useEffect, useState } from 'react';
import { Badge, Button, Callout, Card, Icon } from '../../design';
import { injectCss } from '../../design/injectCss';
import { FormulaSexpr } from './FormulaSexpr';
import { LifecycleBadge } from './PoolBrowser';
import {
  ApiError,
  getFormulaPool,
  postPoolTransition,
  type PoolDetail,
  type PoolLifecycleState,
  type PoolTransition,
} from '../../api/runs';

const CSS = `
.qe-pr { max-width: var(--content-max); margin: 0 auto; padding: 18px; display: flex; flex-direction: column; gap: 16px; }
.qe-pr__hd { display: flex; align-items: center; justify-content: space-between; gap: 16px; padding: 16px 18px; background: var(--surface-card); border: 1px solid var(--border-subtle); border-radius: var(--radius-lg); }
.qe-pr__title { display: flex; align-items: center; gap: 10px; flex-wrap: wrap; }
.qe-pr__title h2 { font-size: 18px; font-family: var(--font-display); }
.qe-pr__mode { padding: 10px 14px; border-radius: var(--radius-md); font: 500 12px var(--font-mono); border: 1px solid var(--border-subtle); }
.qe-pr__mode--sandbox { background: var(--surface-inset); color: var(--text-secondary); }
.qe-pr__mode--production { background: var(--warn-fill-soft, rgba(180,120,0,.14)); color: var(--warn-500, #d08700); border-color: var(--warn-500, #d08700); }
.qe-pr__defl { display: grid; grid-template-columns: repeat(2, 1fr); gap: 1px; background: var(--border-subtle); border: 1px solid var(--border-subtle); border-radius: var(--radius-md); overflow: hidden; }
.qe-pr__defl .m { background: var(--surface-card); padding: 12px 14px; display: flex; flex-direction: column; gap: 3px; }
.qe-pr__defl .m .k { font: 500 10px var(--font-mono); text-transform: uppercase; letter-spacing: .06em; color: var(--text-muted); }
.qe-pr__defl .m .v { font-family: var(--font-mono); font-variant-numeric: tabular-nums; font-size: 15px; font-weight: 600; color: var(--text-primary); }
.qe-pr__defl .m .note { font: 500 10px var(--font-mono); color: var(--text-tertiary); }
.qe-pr__lineage { display: grid; grid-template-columns: 1fr 1fr; gap: 10px; }
.qe-pr__lineage .row { display: flex; flex-direction: column; gap: 2px; }
.qe-pr__lineage .row .k { font: 500 10px var(--font-mono); text-transform: uppercase; letter-spacing: .06em; color: var(--text-muted); }
.qe-pr__lineage .row .v { font-family: var(--font-mono); font-size: 12px; color: var(--text-secondary); overflow: hidden; text-overflow: ellipsis; }
.qe-pr__formulas { display: flex; flex-direction: column; gap: 14px; }
.qe-pr__actions { display: flex; gap: 10px; flex-wrap: wrap; }
.qe-pr__hist { display: flex; flex-direction: column; gap: 6px; }
.qe-pr__hist .h { display: flex; align-items: center; gap: 8px; font: 500 12px var(--font-mono); color: var(--text-secondary); }
.qe-pr__back { margin-bottom: 4px; }
`;

injectCss('qe-pr-css', CSS);

export interface PoolReviewProps {
  poolId: string;
  onBack: () => void;
}

/** Whether `transition` is a legal edge out of `state` (mirrors the server `PoolLifecycleState::apply`). */
function isLegal(state: PoolLifecycleState, transition: PoolTransition): boolean {
  switch (transition) {
    case 'approve':
    case 'reject':
      return state === 'draft';
    case 'seal':
      return state === 'approved';
    case 'revoke':
      return state === 'approved' || state === 'sealed';
    default:
      return false;
  }
}

/**
 * PoolReview (QE-453 screen 4) — **THE GOVERNANCE GATE**. Renders a pool's K formulas (via
 * {@link FormulaSexpr}), the non-collapsible **Deflation-basis card** (the four honest numbers together,
 * never a lone green tile), the review lineage, the current lifecycle state, and the append-only transition
 * history. Approve / Reject / Revoke / Seal are wired to the governance endpoints, each **disabled when
 * illegal from the current state** (mirrors the server state machine: Seal only from `approved`).
 *
 * **Fail-closed honesty (§13.6):** the client never pre-empts the server verdict. A production Seal returns
 * `409 "gated on QE-454"`; the screen surfaces the exact server message in a danger `Callout` and
 * **re-fetches the pool**, so the rendered lifecycle reflects server truth (it stays `approved`) — it never
 * fakes a seal. A sandbox seal proceeds and the returned `sealed` state is reflected.
 */
export function PoolReview({ poolId, onBack }: PoolReviewProps) {
  const [detail, setDetail] = useState<PoolDetail | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [actionError, setActionError] = useState<string | null>(null);
  const [pending, setPending] = useState<PoolTransition | null>(null);

  const load = useCallback(async () => {
    try {
      setDetail(await getFormulaPool(poolId));
      setLoadError(null);
    } catch (e) {
      setLoadError(e instanceof ApiError ? e.message : 'Failed to load the pool.');
    }
  }, [poolId]);

  useEffect(() => {
    void load();
  }, [load]);

  const act = async (transition: PoolTransition) => {
    setActionError(null);
    setPending(transition);
    try {
      await postPoolTransition(poolId, transition);
      // Re-read the authoritative state (also picks up the appended history entry).
      await load();
    } catch (e) {
      // Fail-closed: surface the server's named-blocker message (e.g. the production-seal 409) and
      // re-read so the rendered lifecycle reflects the true, unchanged server state — never fake success.
      setActionError(e instanceof ApiError ? e.message : `Failed to ${transition} the pool.`);
      await load();
    } finally {
      setPending(null);
    }
  };

  if (loadError && !detail) {
    return (
      <div className="qe-pr">
        <div className="qe-pr__back">
          <Button variant="ghost" size="sm" onClick={onBack} iconLeft={<Icon name="arrow-left" size={15} />}>
            All pools
          </Button>
        </div>
        <Callout variant="danger" title="Could not load the pool">
          {loadError}
        </Callout>
      </div>
    );
  }

  if (!detail) {
    return (
      <div className="qe-pr">
        <div className="qe-pr__back">
          <Button variant="ghost" size="sm" onClick={onBack} iconLeft={<Icon name="arrow-left" size={15} />}>
            All pools
          </Button>
        </div>
        <Card>
          <div style={{ padding: 24, textAlign: 'center', color: 'var(--text-tertiary)' }}>Loading pool…</div>
        </Card>
      </div>
    );
  }

  const { content, lifecycle, history } = detail;
  const d = content.deflation;
  const mode = content.mode;
  const isProduction = mode === 'production';
  // The QE-439 "blind floor" tell.
  const blindFloor = d.distinct_evaluations <= d.analytic_floor;

  const ACTIONS: { t: PoolTransition; label: string; icon: string; variant: 'primary' | 'secondary' | 'danger' }[] = [
    { t: 'approve', label: 'Approve', icon: 'check', variant: 'primary' },
    { t: 'seal', label: 'Seal', icon: 'lock', variant: 'primary' },
    { t: 'reject', label: 'Reject', icon: 'ban', variant: 'secondary' },
    { t: 'revoke', label: 'Revoke', icon: 'rotate-ccw', variant: 'danger' },
  ];

  return (
    <div className="qe-pr">
      <div className="qe-pr__back">
        <Button variant="ghost" size="sm" onClick={onBack} iconLeft={<Icon name="arrow-left" size={15} />}>
          All pools
        </Button>
      </div>

      <div className="qe-pr__hd">
        <div className="qe-pr__title">
          <h2>Pool review</h2>
          <LifecycleBadge state={lifecycle} />
          <Badge variant={isProduction ? 'warn' : 'neutral'}>{mode.toUpperCase()}</Badge>
          {!d.gp_aware && <Badge variant="warn">NOT GP-AWARE</Badge>}
        </div>
      </div>

      <div className={`qe-pr__mode qe-pr__mode--${isProduction ? 'production' : 'sandbox'}`} role="note">
        {isProduction
          ? 'PRODUCTION — sealing is fail-closed: the server refuses a production seal until the QE-454 governance gate (seal_allowed / DEFLATION_BASIS_VERSION) lands.'
          : 'RESEARCH (sandbox) — this pool can never reach a production vintage.'}
      </div>

      {actionError && (
        <Callout variant="danger" title="The server refused the action">
          {actionError}
        </Callout>
      )}

      <Card title="Deflation basis (necessary — not sufficient)">
        {blindFloor && (
          <Callout variant="warn" title="Trial basis is the blind analytic floor">
            The distinct-canonical count does not exceed the analytic floor — QE-439 is not driving the
            basis. A production seal blocks on this.
          </Callout>
        )}
        <div className="qe-pr__defl" style={{ marginTop: blindFloor ? 12 : 0 }}>
          {(
            [
              ['Distinct-canonical N', String(d.distinct_evaluations), 'includes rejects'],
              ['Analytic floor', String(d.analytic_floor), blindFloor ? 'N == floor ⇒ blind' : 'cells·gens·windows'],
              ['Trial basis N', String(d.n_trials), 'max(distinct, floor)'],
              ['E[max Sharpe] bar', d.expected_max_sharpe, 'finite via log-N path'],
              ['Uncensored PBO', d.uncensored_pbo ?? '—', `over ${d.variance_trials} trials`],
              ['DSR (champion)', d.champion_dsr, 'necessary, weak √(2 ln N)'],
            ] as const
          ).map(([k, v, note]) => (
            <div className="m" key={k}>
              <span className="k">{k}</span>
              <span className="v">{v}</span>
              <span className="note">{note}</span>
            </div>
          ))}
        </div>
      </Card>

      <Card title={`Frozen formulas (K = ${content.formulas.length})`}>
        <div className="qe-pr__formulas">
          {content.formulas.map((f, i) => (
            <FormulaSexpr key={f.formula_hash} formula={f} index={i + 1} />
          ))}
        </div>
      </Card>

      <Card title="Lineage">
        <div className="qe-pr__lineage">
          {(
            [
              ['Campaign', content.lineage.campaign_id],
              ['Seed', String(content.lineage.seed)],
              ['Code commit', content.lineage.code_commit],
              ['Config hash', content.lineage.config_hash],
              ['Pool hash', content.lineage.pool_hash],
              ['Input snapshot', content.lineage.input_snapshot_id || '—'],
              ['Content hash', detail.content_hash],
              ['Format version', String(content.format_version)],
            ] as const
          ).map(([k, v]) => (
            <div className="row" key={k}>
              <span className="k">{k}</span>
              <span className="v" title={v}>
                {v}
              </span>
            </div>
          ))}
        </div>
      </Card>

      <Card title="Governance">
        <div className="qe-pr__actions">
          {ACTIONS.map((a) => {
            const legal = isLegal(lifecycle, a.t);
            if (!legal) return null; // omit actions illegal from the current state (mirror the machine)
            return (
              <Button
                key={a.t}
                variant={a.variant}
                loading={pending === a.t}
                disabled={pending != null}
                onClick={() => void act(a.t)}
                iconLeft={<Icon name={a.icon} size={15} />}
              >
                {a.label}
                {a.t === 'seal' && isProduction ? ' (gated)' : ''}
              </Button>
            );
          })}
          {lifecycle === 'rejected' || lifecycle === 'revoked' ? (
            <span style={{ fontSize: 13, color: 'var(--text-muted)' }}>
              This pool is in a terminal lifecycle state — no further governance action is possible.
            </span>
          ) : null}
        </div>
        {lifecycle === 'approved' && isProduction && (
          <div style={{ marginTop: 10, fontSize: 12, color: 'var(--text-tertiary)' }}>
            Seal is offered because the state machine allows it, but the server is the authority: a production
            seal will be refused with a 409 until QE-454. The refusal is surfaced above, never hidden.
          </div>
        )}
      </Card>

      <Card title="Transition history">
        {history.length === 0 ? (
          <div style={{ fontSize: 13, color: 'var(--text-muted)' }}>No governance actions recorded yet.</div>
        ) : (
          <div className="qe-pr__hist">
            {history.map((h, i) => (
              <div className="h" key={i}>
                <Icon name="hash" size={13} />
                <span>
                  {h.transition} — {h.actor} — {h.from} → {h.to}
                </span>
              </div>
            ))}
          </div>
        )}
      </Card>
    </div>
  );
}
