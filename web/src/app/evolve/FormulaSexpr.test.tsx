import { describe, it, expect } from 'vitest';
import { render, screen } from '@testing-library/react';
import { FormulaSexpr } from './FormulaSexpr';

describe('FormulaSexpr', () => {
  it('renders the canonical S-expression and a truncated formula hash', () => {
    render(
      <FormulaSexpr
        index={1}
        formula={{ sexpr: 'rank(delta(close)/roll_std(close,20),50)', formula_hash: 'b'.repeat(64) }}
      />,
    );
    expect(screen.getByText('rank(delta(close)/roll_std(close,20),50)')).toBeInTheDocument();
    expect(screen.getByLabelText('formula 1 s-expression')).toBeInTheDocument();
    // The hash is truncated (16 chars + ellipsis), never the full 64.
    expect(screen.getByText(`${'b'.repeat(16)}…`)).toBeInTheDocument();
  });
});
