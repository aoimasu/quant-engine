import {
  Activity,
  ArrowLeft,
  ArrowRight,
  Ban,
  Check,
  Circle,
  Database,
  FlaskConical,
  GitBranch,
  Hash,
  Layers,
  LayoutDashboard,
  ListChecks,
  Lock,
  OctagonX,
  Play,
  Plus,
  RotateCcw,
  Search,
  Shield,
  Sprout,
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
  activity: Activity,
  'arrow-left': ArrowLeft,
  'arrow-right': ArrowRight,
  ban: Ban,
  check: Check,
  circle: Circle,
  database: Database,
  'flask-conical': FlaskConical,
  'git-branch': GitBranch,
  hash: Hash,
  layers: Layers,
  'layout-dashboard': LayoutDashboard,
  'list-checks': ListChecks,
  lock: Lock,
  'octagon-x': OctagonX,
  play: Play,
  plus: Plus,
  'rotate-ccw': RotateCcw,
  search: Search,
  shield: Shield,
  sprout: Sprout,
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
