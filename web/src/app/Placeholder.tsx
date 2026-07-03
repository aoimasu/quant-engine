import { Card, Icon } from '../design';
import { injectCss } from '../design/injectCss';

const CSS = `
.qe-page { max-width: var(--content-max); margin: 0 auto; padding: 24px; }
.qe-empty { display: flex; flex-direction: column; align-items: center; gap: 12px; padding: 56px 24px; text-align: center; color: var(--text-tertiary); }
.qe-empty__ic { width: 40px; height: 40px; border-radius: var(--radius-lg); display: grid; place-items: center; background: var(--accent-fill-soft); color: var(--violet-300); }
.qe-empty__title { font-family: var(--font-display); font-size: var(--fs-lg); font-weight: 600; color: var(--text-primary); }
.qe-empty__sub { font-size: var(--fs-sm); max-width: 42ch; line-height: 1.5; }
.qe-empty__tag { font: 500 10px var(--font-mono); text-transform: uppercase; letter-spacing: .1em; color: var(--text-muted); }
`;

injectCss('qe-placeholder-css', CSS);

export interface PlaceholderProps {
  icon: string;
  title: string;
  description: string;
  /** Ticket that lands the real screen (e.g. "QE-259"). */
  ticket?: string;
}

/** Empty-route placeholder for a nav destination whose screen ships later. */
export function Placeholder({ icon, title, description, ticket }: PlaceholderProps) {
  return (
    <div className="qe-page">
      <Card>
        <div className="qe-empty">
          <div className="qe-empty__ic">
            <Icon name={icon} size={22} />
          </div>
          <div className="qe-empty__title">{title}</div>
          <div className="qe-empty__sub">{description}</div>
          {ticket && <div className="qe-empty__tag">Ships in {ticket}</div>}
        </div>
      </Card>
    </div>
  );
}
