import { useCallback, useEffect, useState } from 'react';
import { Badge, Button, Callout, Card, Icon } from '../../design';
import { injectCss } from '../../design/injectCss';
import {
  ApiError,
  getVintage,
  type ChromosomeComposition,
  type DataProvenance,
  type IndicatorRef,
  type RegimeShare,
  type SealEvidence,
  type VintageDetail,
} from '../../api/runs';

const CSS = `
.qe-vi { max-width: var(--content-max); margin: 0 auto; padding: 18px; display: flex; flex-direction: column; gap: 16px; }
.qe-vi__hd { display: flex; align-items: center; justify-content: space-between; gap: 16px; padding: 16px 18px; background: var(--surface-card); border: 1px solid var(--border-subtle); border-radius: var(--radius-lg); }
.qe-vi__title { display: flex; align-items: center; gap: 10px; flex-wrap: wrap; }
.qe-vi__title h2 { font-size: 18px; font-family: var(--font-display); }
.qe-vi__id { font: 500 12px var(--font-mono); color: var(--text-tertiary); }
.qe-vi__prov { display: flex; align-items: flex-start; gap: 12px; padding: 14px 16px; border-radius: var(--radius-md); border: 1px solid var(--border-subtle); }
.qe-vi__prov .ic { display: inline-flex; margin-top: 1px; }
.qe-vi__prov .tx { display: flex; flex-direction: column; gap: 2px; }
.qe-vi__prov .tag { font: 600 12px var(--font-mono); text-transform: uppercase; letter-spacing: .08em; }
.qe-vi__prov .msg { font-size: 13px; }
.qe-vi__prov--real { background: var(--surface-inset); color: var(--text-secondary); }
.qe-vi__prov--real .tag { color: var(--text-secondary); }
.qe-vi__prov--synthetic { background: var(--warn-fill-soft, rgba(180,120,0,.14)); color: var(--warn-500, #d08700); border-color: var(--warn-500, #d08700); }
.qe-vi__prov--mixed { background: var(--warn-fill-soft, rgba(180,120,0,.14)); color: var(--warn-500, #d08700); border-color: var(--warn-500, #d08700); }
.qe-vi__grid { display: grid; grid-template-columns: repeat(3, 1fr); gap: 1px; background: var(--border-subtle); border: 1px solid var(--border-subtle); border-radius: var(--radius-md); overflow: hidden; }
.qe-vi__grid--2 { grid-template-columns: repeat(2, 1fr); }
.qe-vi__grid .m { background: var(--surface-card); padding: 12px 14px; display: flex; flex-direction: column; gap: 3px; }
.qe-vi__grid .m .k { font: 500 10px var(--font-mono); text-transform: uppercase; letter-spacing: .06em; color: var(--text-muted); }
.qe-vi__grid .m .v { font-family: var(--font-mono); font-variant-numeric: tabular-nums; font-size: 15px; font-weight: 600; color: var(--text-primary); }
.qe-vi__grid .m .note { font: 500 10px var(--font-mono); color: var(--text-tertiary); }
.qe-vi__lead { font: 600 11px var(--font-mono); text-transform: uppercase; letter-spacing: .06em; color: var(--text-secondary); margin: 0 0 8px; }
.qe-vi__demoted { margin-top: 14px; }
.qe-vi__comp { display: flex; flex-direction: column; gap: 12px; }
.qe-vi__chrom { border: 1px solid var(--border-subtle); border-radius: var(--radius-md); padding: 12px 14px; display: flex; flex-direction: column; gap: 8px; }
.qe-vi__chrom .top { display: flex; align-items: center; justify-content: space-between; gap: 10px; }
.qe-vi__chrom .top .cx { font: 600 12px var(--font-mono); color: var(--text-secondary); }
.qe-vi__chrom .top .wt { font-family: var(--font-mono); font-variant-numeric: tabular-nums; font-size: 13px; color: var(--text-primary); }
.qe-vi__inds { display: flex; flex-wrap: wrap; gap: 6px; }
.qe-vi__ind { display: inline-flex; align-items: center; gap: 6px; padding: 3px 8px; border-radius: var(--radius-sm); border: 1px solid var(--border-subtle); font: 500 11px var(--font-mono); color: var(--text-secondary); }
.qe-vi__sel { display: flex; flex-wrap: wrap; gap: 6px; }
.qe-vi__lineage { display: grid; grid-template-columns: 1fr 1fr; gap: 10px; }
.qe-vi__lineage .row { display: flex; flex-direction: column; gap: 2px; }
.qe-vi__lineage .row .k { font: 500 10px var(--font-mono); text-transform: uppercase; letter-spacing: .06em; color: var(--text-muted); }
.qe-vi__lineage .row .v { font-family: var(--font-mono); font-size: 12px; color: var(--text-secondary); overflow: hidden; text-overflow: ellipsis; }
.qe-vi__regimes { display: flex; flex-direction: column; gap: 4px; }
.qe-vi__regimes .r { display: flex; align-items: center; gap: 10px; font: 500 12px var(--font-mono); color: var(--text-secondary); }
.qe-vi__regimes .r .lbl { min-width: 120px; color: var(--text-primary); }
.qe-vi__regimes .r .bars { color: var(--text-tertiary); }
.qe-vi__empty { font-size: 13px; color: var(--text-muted); }
.qe-vi__back { margin-bottom: 4px; }
`;

injectCss('qe-vi-css', CSS);

/** Format a float verbatim for display (no gate recomputation — only presentation). */
function num(v: number | null | undefined, dp = 3): string {
  if (v == null || Number.isNaN(v)) return '—';
  return v.toFixed(dp);
}

/** Format a USD capacity figure for display. */
function usd(v: number | null | undefined): string {
  if (v == null || Number.isNaN(v)) return '—';
  return `$${Math.round(v).toLocaleString('en-US')}`;
}

/** The per-provenance banner copy — `synthetic`/`mixed` are unmistakable; `mixed` is never softened to real. */
const PROVENANCE: Record<DataProvenance, { tag: string; icon: string; msg: string }> = {
  real: {
    tag: 'Real market data',
    icon: 'shield',
    msg: 'Trained and validated on real market bars.',
  },
  synthetic: {
    tag: 'Synthetic data — NOT REAL',
    icon: 'flask-conical',
    msg: 'This vintage was derived from deterministic SYNTHETIC data (qe ingest --synthetic). Its verdict does not reflect real market behaviour — never read it as a real-data result.',
  },
  mixed: {
    tag: 'Mixed real + synthetic',
    icon: 'layers',
    msg: 'This vintage was derived from a LABELLED MIX of real and synthetic data. It is NOT a pure real-data vintage — its coverage includes synthetic bars.',
  },
};

/**
 * ProvenanceBanner — the first-class `data_provenance` banner. A synthetic- or mixed-derived vintage is made
 * UNMISTAKABLE (loud warning styling + explicit copy), mirroring the CLI `"synthetic":true` honesty; a
 * `mixed` vintage is called out distinctly and never softened to `real`.
 */
export function ProvenanceBanner({ provenance }: { provenance: DataProvenance }) {
  const p = PROVENANCE[provenance];
  return (
    <div className={`qe-vi__prov qe-vi__prov--${provenance}`} role="note" aria-label="Data provenance">
      <span className="ic">
        <Icon name={p.icon} size={18} />
      </span>
      <span className="tx">
        <span className="tag">{p.tag}</span>
        <span className="msg">{p.msg}</span>
      </span>
    </div>
  );
}

/** One indicator reference rendered as a chip, labelling catalogue vs evolved provenance. */
function IndicatorChip({ ind }: { ind: IndicatorRef }) {
  return (
    <span className="qe-vi__ind" title={ind.id ?? `feature ${ind.feature}`}>
      <span>{ind.id ?? `evolved #${ind.feature}`}</span>
      <Badge variant={ind.source === 'catalogue' ? 'neutral' : 'info'}>{ind.source.toUpperCase()}</Badge>
    </span>
  );
}

/** The gate-evidence card body — leads net-of-cost/tradability, demotes the deflation basis. */
function GateEvidence({ e, consultations }: { e: SealEvidence; consultations: number }) {
  const tradability = [
    ['Cost-stress net min{1×,2×}', num(e.cost_stress_net_min), 'deployed, net-of-cost'],
    ['Realised turnover', num(e.realised_turnover, 4), 'deployed capacity-capped'],
    ['Capacity (USD)', usd(e.capacity_usd), 'modelled deployable'],
    ['Consultations', String(consultations), 'overlap-keyed per-holdout'],
  ] as const;
  const deflation = [
    ['DSR', num(e.dsr), 'necessary — not sufficient'],
    ['Uncensored PBO', num(e.uncensored_pbo), 'over its trial population'],
    ['PBO (CSCV)', num(e.pbo), 'backtest-overfit prob.'],
    ['Distinct-trial N', String(e.n_trials), 'vs E[maxSharpe] bar'],
    ['SPA p-value', num(e.spa_pvalue), "White's reality check"],
    ['IC / FDR', `${num(e.ic)} / ${num(e.fdr)}`, 'rank-IC / BH level'],
  ] as const;

  return (
    <>
      <p className="qe-vi__lead">Net-of-cost / tradability (leads)</p>
      <div className="qe-vi__grid qe-vi__grid--2">
        {tradability.map(([k, v, note]) => (
          <div className="m" key={k}>
            <span className="k">{k}</span>
            <span className="v">{v}</span>
            <span className="note">{note}</span>
          </div>
        ))}
      </div>

      <p className="qe-vi__lead qe-vi__demoted">Deflation basis (honest — demoted, no lone health tile)</p>
      <div className="qe-vi__grid">
        {deflation.map(([k, v, note]) => (
          <div className="m" key={k}>
            <span className="k">{k}</span>
            <span className="v">{v}</span>
            <span className="note">{note}</span>
          </div>
        ))}
      </div>
    </>
  );
}

/**
 * The standing **"backtest-holdout only — not paper-confirmed"** label (QE-457). Extracted so the composite
 * flow result ({@link import('../training/FlowMonitor').FlowMonitor}) surfaces the identical verdict framing:
 * a flow / vintage verdict is a backtest-holdout evaluation still owing the G2 (shadow/paper) and G3 (live)
 * gates — never read as paper- or live-confirmed.
 */
export function NotPaperConfirmedCallout() {
  return (
    <Callout variant="warn" title="Backtest-holdout only — not paper-confirmed">
      This verdict is a backtest-holdout evaluation. It still owes the G2 (shadow/paper) and G3 (live) gates
      before any promotion — it is not paper-confirmed and not live-confirmed.
    </Callout>
  );
}

/**
 * The frozen-holdout **regime composition** chips (QE-125/QE-460) — which regimes the frozen holdout spans
 * (rode diverse regimes, not one trailing block). Extracted so the composite flow result mirrors the
 * Inspector's regime rendering verbatim from the same persisted `regime_composition`.
 */
export function RegimeComposition({ regimes }: { regimes: RegimeShare[] }) {
  const totalRegimeBars = regimes.reduce((a, r) => a + r.bars, 0);
  return (
    <>
      <p className="qe-vi__lead">Holdout regime composition (rode diverse regimes, not one trailing block)</p>
      {regimes.length === 0 ? (
        <div className="qe-vi__empty">Regime composition not yet recorded for this vintage (QE-460).</div>
      ) : (
        <div className="qe-vi__regimes">
          {regimes.map((r) => (
            <div className="r" key={r.regime}>
              <span className="lbl">{r.regime}</span>
              <span className="bars">{r.bars} bars</span>
              <span>{totalRegimeBars > 0 ? `${((r.bars / totalRegimeBars) * 100).toFixed(1)}%` : '—'}</span>
            </div>
          ))}
        </div>
      )}
    </>
  );
}

export interface VintageInspectorProps {
  vintageId: string;
  onBack: () => void;
}

/**
 * VintageInspector (QE-457) — the read-only inspection screen for one sealed vintage, mirroring the evolve
 * {@link import('../evolve/PoolReview').PoolReview} screen. It surfaces the QE-467-persisted evidence that
 * `GET /api/vintages/{id}` (QE-456) exposes: a first-class {@link ProvenanceBanner}, the ensemble composition
 * (chromosome → referenced indicators → weight), the union of selected indicators, a gate-evidence card that
 * **leads with the net-of-cost / tradability numbers** and shows the honest deflation basis (never a lone
 * green tile), and the frozen-holdout split + its regime composition. It carries a standing
 * "backtest-holdout only — not paper-confirmed" label.
 *
 * **Inspection only.** The vintage is already sealed by the train gate, so the screen renders **no**
 * seal / promote / select / approve / revoke affordance — nothing mutates server state. Every number is
 * rendered verbatim from the payload; the client recomputes no gate.
 */
export function VintageInspector({ vintageId, onBack }: VintageInspectorProps) {
  const [detail, setDetail] = useState<VintageDetail | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);

  const load = useCallback(async () => {
    try {
      setDetail(await getVintage(vintageId));
      setLoadError(null);
    } catch (e) {
      setLoadError(e instanceof ApiError ? e.message : 'Failed to load the vintage.');
    }
  }, [vintageId]);

  useEffect(() => {
    void load();
  }, [load]);

  const back = (
    <div className="qe-vi__back">
      <Button variant="ghost" size="sm" onClick={onBack} iconLeft={<Icon name="arrow-left" size={15} />}>
        All vintages
      </Button>
    </div>
  );

  if (loadError && !detail) {
    return (
      <div className="qe-vi">
        {back}
        <Callout variant="danger" title="Could not load the vintage">
          {loadError}
        </Callout>
      </div>
    );
  }

  if (!detail) {
    return (
      <div className="qe-vi">
        {back}
        <Card>
          <div style={{ padding: 24, textAlign: 'center', color: 'var(--text-tertiary)' }}>Loading vintage…</div>
        </Card>
      </div>
    );
  }

  // The union of indicators the ensemble references (deduped by display id, catalogue vs evolved preserved).
  const selected = new Map<string, IndicatorRef>();
  for (const c of detail.composition) {
    for (const ind of c.indicators) {
      selected.set(ind.id ?? `evolved-${ind.feature}`, ind);
    }
  }
  const split = detail.holdout_split;

  return (
    <div className="qe-vi">
      {back}

      <div className="qe-vi__hd">
        <div className="qe-vi__title">
          <h2>Vintage inspector</h2>
          <span className="qe-vi__id">{detail.id}</span>
        </div>
        <Badge variant="neutral">FORMAT v{detail.format_version}</Badge>
      </div>

      <ProvenanceBanner provenance={detail.data_provenance} />

      <NotPaperConfirmedCallout />

      <Card title="Gate evidence (net-of-cost / tradability led)">
        <GateEvidence e={detail.seal_evidence} consultations={detail.consultation_count} />
      </Card>

      <Card title={`Ensemble composition (K = ${detail.composition.length})`}>
        <div className="qe-vi__comp">
          {detail.composition.map((c) => (
            <ChromosomeRow key={c.index} c={c} />
          ))}
        </div>
      </Card>

      <Card title={`Selected indicators (${selected.size})`}>
        {selected.size === 0 ? (
          <div className="qe-vi__empty">No indicators referenced.</div>
        ) : (
          <div className="qe-vi__sel">
            {[...selected.values()].map((ind) => (
              <IndicatorChip key={ind.id ?? `evolved-${ind.feature}`} ind={ind} />
            ))}
          </div>
        )}
      </Card>

      <Card title="Frozen holdout">
        <div className="qe-vi__lineage" style={{ marginBottom: 12 }}>
          {(
            [
              ['Train range', split.train_range ? `${split.train_range.start} → ${split.train_range.end}` : '—'],
              ['Embargo bars', String(split.embargo_bars)],
              [
                'Holdout range',
                split.holdout_range ? `${split.holdout_range.start} → ${split.holdout_range.end}` : '—',
              ],
              ['Holdout series length', `${detail.holdout_series_len} bars`],
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
        <RegimeComposition regimes={detail.regime_composition} />
      </Card>

      <Card title="Lineage">
        <div className="qe-vi__lineage">
          {(
            [
              ['Vintage id', detail.id],
              ['Content hash', detail.content_hash],
              ['Format version', String(detail.format_version)],
              ['Holdout series handle', detail.holdout_series_handle],
              ['Worst-case loss', num(detail.sidecars.worst_case_loss)],
              ['Primary run', detail.primary_run ?? '—'],
              ['Producing runs', String(detail.producing_runs.length)],
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
    </div>
  );
}

/** One chromosome composition row — index, weight, and its referenced indicators. */
function ChromosomeRow({ c }: { c: ChromosomeComposition }) {
  return (
    <div className="qe-vi__chrom">
      <div className="top">
        <span className="cx">Chromosome #{c.index}</span>
        <span className="wt">weight {num(c.weight, 4)}</span>
      </div>
      {c.indicators.length === 0 ? (
        <div className="qe-vi__empty">No referenced indicators.</div>
      ) : (
        <div className="qe-vi__inds">
          {c.indicators.map((ind) => (
            <IndicatorChip key={`${ind.feature}-${ind.id ?? 'evolved'}`} ind={ind} />
          ))}
        </div>
      )}
    </div>
  );
}
