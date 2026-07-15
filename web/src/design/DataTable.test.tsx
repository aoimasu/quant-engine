import { describe, it, expect, vi } from 'vitest';
import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { DataTable } from './DataTable';
import type { Column } from './DataTable';

interface Row extends Record<string, unknown> {
  id: string;
  name: string;
}

const columns: Column<Row>[] = [
  { key: 'id', header: 'ID' },
  { key: 'name', header: 'Name' },
];

const rows: Row[] = [
  { id: 'r1', name: 'Alpha' },
  { id: 'r2', name: 'Beta' },
];

describe('DataTable — keyboard-operable clickable rows (QE-422)', () => {
  it('exposes clickable rows as focusable buttons and activates on Enter AND Space', async () => {
    const onRowClick = vi.fn();
    render(<DataTable columns={columns} rows={rows} keyField="id" onRowClick={onRowClick} />);

    // Each clickable row is a real button in the a11y tree, focusable via tabIndex=0.
    const buttons = screen.getAllByRole('button');
    expect(buttons).toHaveLength(2);
    buttons.forEach((b) => expect(b).toHaveAttribute('tabindex', '0'));

    const firstRow = buttons[0];

    // Keyboard alone: focus the row, press Enter -> handler fires with that row.
    firstRow.focus();
    expect(firstRow).toHaveFocus();
    await userEvent.keyboard('{Enter}');
    expect(onRowClick).toHaveBeenCalledTimes(1);
    expect(onRowClick).toHaveBeenLastCalledWith(rows[0], 0);

    // Space also activates (and preventDefault stops page scroll).
    firstRow.focus();
    await userEvent.keyboard(' ');
    expect(onRowClick).toHaveBeenCalledTimes(2);
    expect(onRowClick).toHaveBeenLastCalledWith(rows[0], 0);

    // A different row activates with its own index.
    buttons[1].focus();
    await userEvent.keyboard('{Enter}');
    expect(onRowClick).toHaveBeenLastCalledWith(rows[1], 1);
  });

  it('leaves rows non-interactive (no role/tabIndex/keydown) when onRowClick is absent', async () => {
    render(<DataTable columns={columns} rows={rows} keyField="id" />);

    // No button role, so no keyboard affordance is advertised.
    expect(screen.queryByRole('button')).toBeNull();

    const bodyRows = document.querySelectorAll('tbody tr');
    expect(bodyRows).toHaveLength(2);
    bodyRows.forEach((tr) => {
      expect(tr).not.toHaveAttribute('role');
      expect(tr).not.toHaveAttribute('tabindex');
      expect(tr).not.toHaveClass('qe-table__row--clickable');
    });

    // Pressing keys on a focused non-interactive row does nothing (no handler wired).
    (bodyRows[0] as HTMLElement).focus();
    await userEvent.keyboard('{Enter} ');
    // Nothing to assert firing on; the absence of role/tabIndex above proves it is inert.
    expect(screen.queryByRole('button')).toBeNull();
  });
});
