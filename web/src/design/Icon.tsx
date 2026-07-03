import {
  Circle,
  Database,
  FlaskConical,
  GitBranch,
  LayoutDashboard,
  ListChecks,
  Search,
  Shield,
  Terminal,
  Wallet,
  type LucideIcon,
  type LucideProps,
} from 'lucide-react';

/**
 * Icon — thin wrapper over Lucide.
 *
 * The Claude Design source loaded Lucide from a CDN (`window.lucide`) and used
 * kebab-case names: `<Icon name="trending-up" />`. To avoid a runtime CDN we
 * render the bundled `lucide-react` package, keeping the exact same kebab-case
 * `name` API. Only the glyphs actually used by the app are imported here so the
 * bundle stays tree-shaken (importing the full registry pulls in ~1,500 icons).
 * Add a glyph to REGISTRY when a new `name` is needed.
 */
export interface IconProps extends Omit<LucideProps, 'ref'> {
  name: string;
  size?: number;
  strokeWidth?: number;
}

const REGISTRY: Record<string, LucideIcon> = {
  circle: Circle,
  database: Database,
  'flask-conical': FlaskConical,
  'git-branch': GitBranch,
  'layout-dashboard': LayoutDashboard,
  'list-checks': ListChecks,
  search: Search,
  shield: Shield,
  terminal: Terminal,
  wallet: Wallet,
};

export function Icon({ name, size = 18, strokeWidth = 2, ...rest }: IconProps) {
  const LucideGlyph = REGISTRY[name];
  if (!LucideGlyph) {
    // Unknown glyph: render a neutral placeholder box the size of the icon so
    // layout is preserved (matches the CDN wrapper's graceful failure mode).
    return (
      <span
        aria-hidden="true"
        style={{ width: size, height: size, display: 'inline-flex', flex: 'none' }}
      />
    );
  }
  return <LucideGlyph size={size} strokeWidth={strokeWidth} aria-hidden="true" {...rest} />;
}
