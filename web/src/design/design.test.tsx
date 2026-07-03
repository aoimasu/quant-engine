import { describe, it, expect } from 'vitest';
import { render, screen } from '@testing-library/react';
import { Button } from './Button';
import { Badge } from './Badge';
import { Callout } from './Callout';
import { Icon } from './Icon';

describe('design primitives', () => {
  it('Button renders with the design class and variant modifier', () => {
    render(<Button variant="primary">Run backtest</Button>);
    const btn = screen.getByRole('button', { name: /run backtest/i });
    expect(btn).toHaveClass('qe-btn', 'qe-btn--primary');
  });

  it('Button is disabled while loading', () => {
    render(<Button loading>Wait</Button>);
    expect(screen.getByRole('button')).toBeDisabled();
  });

  it('Badge renders with its variant class', () => {
    render(<Badge variant="accent">LIVE</Badge>);
    expect(screen.getByText('LIVE')).toHaveClass('qe-badge', 'qe-badge--accent');
  });

  it('Callout renders title and body with the variant class', () => {
    const { container } = render(
      <Callout variant="danger" title="Access denied">
        Not allowlisted
      </Callout>,
    );
    expect(container.querySelector('.qe-callout--danger')).not.toBeNull();
    expect(screen.getByText('Access denied')).toBeInTheDocument();
    expect(screen.getByText('Not allowlisted')).toBeInTheDocument();
  });

  it('Icon renders a bundled Lucide SVG for a known kebab-case name', () => {
    const { container } = render(<Icon name="database" size={20} />);
    const svg = container.querySelector('svg');
    expect(svg).not.toBeNull();
    expect(svg).toHaveClass('lucide');
  });

  it('Icon renders a sized placeholder for an unknown name (no crash)', () => {
    const { container } = render(<Icon name="definitely-not-an-icon" size={16} />);
    expect(container.querySelector('svg')).toBeNull();
    const span = container.querySelector('span');
    expect(span).not.toBeNull();
  });
});
