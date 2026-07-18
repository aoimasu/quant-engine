import { injectCss } from '../../design/injectCss';
import type { PoolFormula } from '../../api/runs';

const CSS = `
.qe-fx { display: flex; flex-direction: column; gap: 6px; }
.qe-fx__code { overflow-x: auto; padding: 10px 12px; background: var(--surface-inset); border: 1px solid var(--border-subtle); border-radius: var(--radius-md); }
.qe-fx__code code { font-family: var(--font-mono); font-size: 13px; color: var(--text-primary); white-space: pre; }
.qe-fx__meta { display: flex; align-items: center; gap: 8px; font: 500 10px var(--font-mono); color: var(--text-muted); }
.qe-fx__meta .k { text-transform: uppercase; letter-spacing: .06em; }
.qe-fx__hash { color: var(--text-tertiary); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
`;

injectCss('qe-fx-css', CSS);

export interface FormulaSexprProps {
  formula: PoolFormula;
  /** Optional 1-based index shown as a label (`f{n}`). */
  index?: number;
}

/**
 * FormulaSexpr (QE-453 screen 3) — renders one evolved formula's **canonical S-expression** readably: the
 * single-line canonical form in an `overflow-x:auto` mono container (so a long formula scrolls inside its
 * own box, never the page) plus the content-addressed `formula_hash`. This is the human-inspectable form a
 * reviewer reads before approving a pool; PoolReview renders one per pooled formula.
 */
export function FormulaSexpr({ formula, index }: FormulaSexprProps) {
  return (
    <div className="qe-fx">
      <div className="qe-fx__code" aria-label={index != null ? `formula ${index} s-expression` : 's-expression'}>
        <code>{formula.sexpr}</code>
      </div>
      <div className="qe-fx__meta">
        {index != null && <span className="k">f{index}</span>}
        <span className="k">hash</span>
        <span className="qe-fx__hash" title={formula.formula_hash}>
          {formula.formula_hash.slice(0, 16)}…
        </span>
      </div>
    </div>
  );
}
